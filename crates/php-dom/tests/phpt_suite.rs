//! Runs the php-src `ext/dom` `.phpt` tests through a tiny PHP interpreter
//! (`tests/minphp`) that calls into this crate via `php_dom::dispatch`.
//!
//! The test directory is copied to `target/` first (tests write temporary
//! files next to themselves and php-src is read-only). A report with every
//! result is written to `target/phpt-dom-report.txt`; set
//! `PHPT_FILTER=substring` to run a subset and `PHPT_VERBOSE=1` to print
//! diffs of failures.

mod minphp;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use phpt_runner::expectf::{expectf_matches, regex_matches};
use phpt_runner::phpt::{ExpectKind, TestFile};

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("mkdir");
    for e in std::fs::read_dir(src).expect("read_dir") {
        let e = e.expect("entry");
        let p = e.path();
        let t = dst.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &t);
        } else if !t.exists() {
            std::fs::copy(&p, &t).expect("copy");
        }
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("read_dir")
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|e| e == "phpt") {
            out.push(p);
        }
    }
}

fn trim(b: &[u8]) -> Vec<u8> {
    let s = String::from_utf8_lossy(b).replace("\r\n", "\n");
    s.trim().as_bytes().to_vec()
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Pass,
    Fail(String),
    Skip(String),
}

fn run_php(file: &Path, src: &[u8]) -> Vec<u8> {
    php_libxml::error::reset_request_state();
    minphp::value::reset_ids();
    let i = minphp::interp::Interp::new(&file.to_string_lossy());
    i.run(src);
    let out = i.out.borrow().clone();
    out
}

fn run_one(path: &Path) -> Outcome {
    let test = match TestFile::load(path, false) {
        Ok(t) => t,
        Err(e) => return Outcome::Skip(format!("bork: {e}")),
    };
    if let Some(ext) = test.section("EXTENSIONS") {
        let s = String::from_utf8_lossy(ext);
        for e in s.split_whitespace() {
            if !matches!(e, "dom" | "libxml" | "xml") {
                return Outcome::Skip(format!("needs extension {e}"));
            }
        }
    }
    let php_path = path.with_extension("php");
    if let Some(env) = test.section("ENV") {
        for line in String::from_utf8_lossy(env).lines() {
            if let Some((k, v)) = line.split_once('=') {
                // SAFETY-free: tests run on a single thread.
                std::env::set_var(k.trim(), v.trim());
            }
        }
    }
    if let Some(skip) = test.section("SKIPIF") {
        let out = run_php(&php_path, skip);
        let o = String::from_utf8_lossy(&out).trim().to_ascii_lowercase();
        if o.starts_with("skip") {
            return Outcome::Skip(o);
        }
    }
    let Some(src) = test.section("FILE") else {
        return Outcome::Skip("no FILE section".into());
    };
    let out = run_php(&php_path, src);
    if let Some(clean) = test.section("CLEAN") {
        let _ = run_php(&php_path, clean);
    }
    let out_s = String::from_utf8_lossy(&out);
    if out_s.contains("Uncaught UnsupportedInTestHarness") {
        return Outcome::Skip("harness limitation".into());
    }
    let Some((kind, wanted)) = test.expectation() else {
        return Outcome::Skip("no expectation".into());
    };
    let got = trim(&out);
    let want = trim(wanted);
    let ok = match kind {
        ExpectKind::Exact => got == want,
        ExpectKind::Format => expectf_matches(&want, &got),
        ExpectKind::Regex => regex_matches(&want, &got),
    };
    if ok {
        Outcome::Pass
    } else {
        Outcome::Fail(format!(
            "---- expected ----\n{}\n---- got ----\n{}\n",
            String::from_utf8_lossy(&want),
            String::from_utf8_lossy(&got)
        ))
    }
}

#[test]
fn phpt_dom_suite() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("../../modules/php-src/ext/dom/tests");
    if !src.exists() {
        eprintln!("php-src checkout missing; skipping");
        return;
    }
    let target = manifest.join("../../target/phpt-dom");
    copy_dir(&src, &target);
    let mut files = Vec::new();
    collect(&target, &mut files);
    let filter = std::env::var("PHPT_FILTER").ok();
    let verbose = std::env::var("PHPT_VERBOSE").is_ok();
    let target_clone = target.clone();
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            // run-tests.php runs scripts from the php-src root, not the
            // test directory: use an empty root with ext/dom/tests linked.
            let root = target_clone.with_file_name("phpt-dom-root");
            let _ = std::fs::create_dir_all(root.join("ext/dom"));
            #[cfg(unix)]
            let _ = std::os::unix::fs::symlink(&target_clone, root.join("ext/dom/tests"));
            let _ = std::env::set_current_dir(&root);
            let mut report = String::new();
            let (mut pass, mut fail, mut skip) = (0, 0, 0);
            for f in &files {
                let name = f
                    .strip_prefix(&target_clone)
                    .unwrap_or(f)
                    .to_string_lossy()
                    .into_owned();
                if let Some(flt) = &filter {
                    if !name.contains(flt.as_str()) {
                        continue;
                    }
                }
                let r = std::panic::catch_unwind(|| run_one(f));
                let r = match r {
                    Ok(o) => o,
                    Err(e) => {
                        let msg = e
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_string()))
                            .unwrap_or_default();
                        Outcome::Fail(format!("PANIC: {msg}"))
                    }
                };
                match &r {
                    Outcome::Pass => {
                        pass += 1;
                        let _ = writeln!(report, "PASS {name}");
                    }
                    Outcome::Fail(d) => {
                        fail += 1;
                        let _ = writeln!(report, "FAIL {name}");
                        let _ =
                            std::fs::write(format!("{}.diff", f.with_extension("").display()), d);
                        if verbose {
                            eprintln!("FAIL {name}\n{d}");
                        }
                    }
                    Outcome::Skip(why) => {
                        skip += 1;
                        let _ = writeln!(report, "SKIP {name} ({why})");
                    }
                }
            }
            let _ = writeln!(report, "\nTOTAL pass={pass} fail={fail} skip={skip}");
            (report, pass, fail, skip)
        })
        .expect("spawn");
    let (report, pass, fail, skip) = handle.join().expect("runner thread");
    let _ = std::fs::write(manifest.join("../../target/phpt-dom-report.txt"), &report);
    eprintln!("phpt ext/dom: pass={pass} fail={fail} skip={skip}");
    let min: usize = std::env::var("PHPT_MIN_PASS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    assert!(pass >= min, "pass count {pass} below {min}");
}
