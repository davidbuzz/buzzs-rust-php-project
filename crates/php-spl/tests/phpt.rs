//! Runs the ext/spl `.phpt` tests through the test interpreter.
//!
//! `phpt_passing` asserts that every test listed in `tests/phpt_pass.txt`
//! still passes. `phpt_survey` (ignored) runs the whole directory, prints a
//! summary and rewrites the list:
//! `cargo test -p php-spl --test phpt -- --ignored --nocapture`.

mod harness;

use std::path::{Path, PathBuf};

fn spl_tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src/ext/spl/tests")
}

fn all_tests() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![spl_tests_dir()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "phpt") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn rel(p: &Path) -> String {
    p.strip_prefix(spl_tests_dir()).unwrap_or(p).to_string_lossy().into_owned()
}

/// A scratch copy of the test directory (tests write files next to
/// themselves; the php-src checkout must stay untouched).
fn work_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dst = std::env::temp_dir().join(format!("php-spl-phpt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dst);
        let src = spl_tests_dir();
        let mut stack = vec![src.clone()];
        while let Some(d) = stack.pop() {
            let rel = d.strip_prefix(&src).unwrap_or(&d).to_path_buf();
            let _ = std::fs::create_dir_all(dst.join(&rel));
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_none_or(|x| x != "phpt") {
                    let _ = std::fs::copy(&p, dst.join(&rel).join(e.file_name()));
                }
            }
        }
        dst
    })
    .clone()
}

fn run_isolated(p: &Path) -> harness::Outcome {
    let p = p.to_path_buf();
    let work = work_dir().join(p.strip_prefix(spl_tests_dir()).unwrap_or(&p));
    let h = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || harness::run_phpt(&p, &work))
        .expect("spawn");
    h.join().unwrap_or(harness::Outcome::Fail { expected: String::new(), got: "panic".into() })
}

#[test]
fn phpt_passing() {
    if !spl_tests_dir().exists() {
        return;
    }
    let list = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/phpt_pass.txt")).unwrap_or_default();
    let mut failures = Vec::new();
    for name in list.lines().filter(|l| !l.trim().is_empty()) {
        let p = spl_tests_dir().join(name);
        if let harness::Outcome::Fail { expected, got } = run_isolated(&p) {
            failures.push(format!("{name}\n--- expected\n{expected}\n--- got\n{got}\n"));
        }
    }
    let _ = std::fs::remove_dir_all(work_dir());
    assert!(failures.is_empty(), "{} regressions:\n{}", failures.len(), failures.join("\n"));
}

#[test]
#[ignore]
fn phpt_survey() {
    let filter = std::env::var("SPL_PHPT_FILTER").unwrap_or_default();
    let mut pass = Vec::new();
    let mut fail = Vec::new();
    let mut skip = 0;
    for p in all_tests() {
        let r = rel(&p);
        if !filter.is_empty() && !r.contains(&filter) {
            continue;
        }
        if std::env::var("SPL_PHPT_TRACE").is_ok() {
            eprintln!("RUN {r}");
        }
        match run_isolated(&p) {
            harness::Outcome::Pass => pass.push(r),
            harness::Outcome::Fail { expected, got } => {
                if std::env::var("SPL_PHPT_VERBOSE").is_ok() {
                    println!("FAIL {r}\n--- expected\n{expected}\n--- got\n{got}\n");
                }
                fail.push(r);
            }
            harness::Outcome::Skip(_) => skip += 1,
        }
    }
    let _ = std::fs::remove_dir_all(work_dir());
    println!("pass {} fail {} skip {}", pass.len(), fail.len(), skip);
    for f in &fail {
        println!("FAIL {f}");
    }
    if filter.is_empty() {
        std::fs::write(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/phpt_pass.txt"),
            pass.join("\n") + "\n",
        )
        .expect("write list");
    }
}

/// Runs `$SPL_RUN_FILE` (a PHP script) through the interpreter and prints
/// the output: `SPL_RUN_FILE=x.php cargo test -p php-spl --test phpt run_file -- --ignored --nocapture`.
#[test]
#[ignore]
fn run_file() {
    let Ok(f) = std::env::var("SPL_RUN_FILE") else { return };
    let src = std::fs::read(&f).expect("read");
    let out = harness::run(&src, &f);
    println!("{}", String::from_utf8_lossy(&out));
}
