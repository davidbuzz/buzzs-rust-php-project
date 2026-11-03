//! Integration tests: the section parser and EXPECTF matcher against fixtures, and end-to-end
//! runs of the `phpt-runner` binary with the reference `/usr/bin/php` (skipped when absent).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use phpt_runner::expectf::{expectf_matches, expectf_to_regex, regex_matches};
use phpt_runner::phpt::{ExpectKind, TestFile};
use phpt_runner::report::JsonReport;
use phpt_runner::result::Status;

const REFERENCE_PHP: &str = "/usr/bin/php";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run")
}

fn php_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src")
}

fn reference_php() -> Option<&'static str> {
    Path::new(REFERENCE_PHP).is_file().then_some(REFERENCE_PHP)
}

/// Copy the fixtures into a private directory (tests write generated files next to themselves).
fn fixture_copy(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("phpt-fixtures-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for entry in fs::read_dir(fixtures()).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dir.join(entry.file_name())).unwrap();
    }
    dir
}

/// Run the binary and return the parsed JSON report and exit code.
fn run_runner(args: &[&str], paths: &[&Path]) -> (JsonReport, Option<i32>, String) {
    let json = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "report-{}-{}.json",
        std::process::id(),
        args.join("_").replace(['/', ' '], "_")
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_phpt-runner"))
        .args(args)
        .arg("--json")
        .arg(&json)
        .args(paths)
        .output()
        .expect("run phpt-runner");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let report: JsonReport = serde_json::from_slice(&fs::read(&json).unwrap_or_else(|e| {
        panic!(
            "no report ({e}); stdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    }))
    .unwrap();
    let _ = fs::remove_file(&json);
    (report, output.status.code(), stdout)
}

#[test]
fn parser_reads_every_fixture() {
    for entry in fs::read_dir(fixtures()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "phpt") {
            continue;
        }
        let parsed = TestFile::load(&path, false);
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("bork_unknown") {
            assert_eq!(parsed.unwrap_err().0, "Unknown section \"NOPE\"");
        } else {
            let t = parsed.unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!t.name().is_empty(), "{name}");
            assert!(t.has_section("FILE"), "{name}");
            assert!(t.expectation().is_some(), "{name}");
        }
    }
}

#[test]
fn parser_sections_of_specific_fixtures() {
    let t = TestFile::load(&fixtures().join("pass_external.phpt"), false).unwrap();
    assert_eq!(
        t.section("FILE").unwrap(),
        b"<?php echo \"from external\\n\";\n"
    );
    assert_eq!(
        t.expectation().unwrap(),
        (ExpectKind::Exact, &b"from external\n"[..])
    );

    let t = TestFile::load(&fixtures().join("pass_done_marker.phpt"), false).unwrap();
    assert_eq!(
        t.section("FILE").unwrap(),
        b"<?php echo \"done\\n\"; ?>\n===DONE===\n"
    );

    let t = TestFile::load(&fixtures().join("skip_cgi.phpt"), false).unwrap();
    assert!(t.is_cgi());
    assert_eq!(t.section("POST").unwrap(), b"a=1\n");

    let t = TestFile::load(&fixtures().join("pass_ini.phpt"), false).unwrap();
    assert_eq!(
        t.section("INI").unwrap(),
        b"precision=5\narg_separator.output==\n"
    );
    assert_eq!(t.section_names(), vec!["EXPECT", "FILE", "INI", "TEST"]);
}

#[test]
fn expectf_matcher_cases() {
    // (pattern, subject, should match)
    let cases: &[(&[u8], &[u8], bool)] = &[
        (b"%s", b"anything here", true),
        (b"%s", b"two\nlines", false),
        (b"%S", b"", true),
        (b"%a", b"multi\nline", true),
        (b"%a", b"", false),
        (b"%A", b"", true),
        (b"%w", b"  \t", true),
        (b"%i", b"+5", true),
        (b"%i", b"-5", true),
        (b"%d", b"-5", false),
        (b"%x", b"DeadBeef", true),
        (b"%x", b"xyz", false),
        (b"%f", b"-1.5E+10", true),
        (b"%f", b"1.", false),
        (b"%c", b"\n", true),
        (b"%e", b"/", true),
        (b"%0", b"\0", true),
        (
            b"literal (%d) [x] {y} $z ^ | ? * + . \\ #",
            b"literal (1) [x] {y} $z ^ | ? * + . \\ #",
            true,
        ),
        (b"a %r(b|c)+%r d", b"a bcb d", true),
        (b"a %r(b|c)+%r d", b"a e d", false),
        (b"%r\\d+%r%s", b"12ab", true),
        (b"unbalanced %r here", b"unbalanced %r here", true),
        (b"line1\r\nline2", b"line1\nline2", true),
        (b"%unknown", b"%unknown", true),
    ];
    for (pattern, subject, expected) in cases {
        assert_eq!(
            expectf_matches(pattern, subject),
            *expected,
            "pattern {:?} vs {:?} (regex {:?})",
            String::from_utf8_lossy(pattern),
            String::from_utf8_lossy(subject),
            String::from_utf8_lossy(&expectf_to_regex(pattern)),
        );
    }
    assert!(regex_matches(b"(?i)HELLO", b"hello"));
    assert!(!regex_matches(b"unescaped/slash", b"unescaped/slash"));
}

#[test]
fn fixtures_against_reference_php() {
    let Some(php) = reference_php() else {
        eprintln!("skipping: {REFERENCE_PHP} not found");
        return;
    };
    let dir = fixture_copy("run");
    let (report, code, stdout) = run_runner(
        &["--php", php, "--timeout", "2", "-j", "4", "--no-color"],
        &[&dir],
    );
    let got: BTreeMap<String, Status> = report
        .tests
        .iter()
        .map(|t| (t.path.rsplit('/').next().unwrap().to_string(), t.result))
        .collect();
    for (name, status) in &got {
        let expected = match name.split('_').next().unwrap() {
            "pass" => Status::Pass,
            "fail" => Status::Fail,
            "skip" => Status::Skip,
            "xfail" => Status::Xfail,
            "warn" => Status::Warn,
            "bork" => Status::Bork,
            other => panic!("unexpected fixture prefix {other}"),
        };
        assert_eq!(*status, expected, "{name}\n{stdout}");
    }
    assert_eq!(got.len(), 23, "{stdout}");
    assert_eq!(code, Some(1), "failures must give a non-zero exit code");
    // Artifacts are written for failures only, and the CLEAN section ran.
    assert!(dir.join("fail_expect.diff").is_file());
    assert!(dir.join("fail_expect.out").is_file());
    assert!(dir.join("fail_expect.exp").is_file());
    assert!(!dir.join("pass_expect.diff").exists());
    assert!(!dir.join("pass_expect.php").exists());
    assert!(!dir.join("clean_marker.tmp").exists());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn redirect_test_against_reference_php() {
    let Some(php) = reference_php() else {
        eprintln!("skipping: {REFERENCE_PHP} not found");
        return;
    };
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/redirect");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("phpt-redirect-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("targets")).unwrap();
    for f in ["redirect.phpt", "targets/one.phpt", "targets/two.phpt"] {
        fs::copy(src.join(f), dir.join(f)).unwrap();
    }
    let (report, code, stdout) = run_runner(
        &["--php", php, "--srcdir", dir.to_str().unwrap()],
        &[&dir.join("redirect.phpt")],
    );
    assert_eq!(code, Some(0), "{stdout}");
    let got: Vec<(&str, &str, Status)> = report
        .tests
        .iter()
        .map(|t| (t.path.as_str(), t.name.as_str(), t.result))
        .collect();
    assert_eq!(
        got,
        [
            ("one.phpt", "Redirector redirected one", Status::Pass),
            ("two.phpt", "Redirector redirected two", Status::Pass),
        ],
        "{stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn baseline_round_trip() {
    let Some(php) = reference_php() else {
        eprintln!("skipping: {REFERENCE_PHP} not found");
        return;
    };
    let dir = fixture_copy("baseline");
    let baseline = dir.join("baseline.txt");
    let b = baseline.to_str().unwrap();
    let fixtures: Vec<PathBuf> = ["fail_expect.phpt", "pass_expect.phpt", "bork_clean.phpt"]
        .iter()
        .map(|f| dir.join(f))
        .collect();
    let paths: Vec<&Path> = fixtures.iter().map(PathBuf::as_path).collect();

    let (_, code, _) = run_runner(
        &["--php", php, "--baseline", b, "--update-baseline"],
        &paths,
    );
    assert_eq!(code, Some(0));
    let text = fs::read_to_string(&baseline).unwrap();
    assert_eq!(
        text.lines().filter(|l| !l.starts_with('#')).count(),
        2,
        "{text}"
    );

    // Same failures as the baseline: success.
    let (_, code, _) = run_runner(&["--php", php, "--baseline", b], &paths);
    assert_eq!(code, Some(0));

    // A baseline entry that now passes is reported and fails the run.
    let pass_name = text
        .lines()
        .find(|l| l.ends_with("fail_expect.phpt"))
        .unwrap();
    fs::write(
        &baseline,
        format!(
            "{pass_name}\n{}\n",
            pass_name.replace("fail_expect", "pass_expect")
        ),
    )
    .unwrap();
    let (_, code, stdout) = run_runner(&["--php", php, "--baseline", b], &paths);
    assert_eq!(code, Some(1));
    assert!(stdout.contains("UNEXPECTED PASS"), "{stdout}");
    assert!(stdout.contains("NEW FAILURE"), "{stdout}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn php_src_run_test_suite_passes() {
    let Some(php) = reference_php() else {
        eprintln!("skipping: {REFERENCE_PHP} not found");
        return;
    };
    let dir = php_src().join("tests/run-test");
    if !dir.is_dir() {
        eprintln!("skipping: {} not found", dir.display());
        return;
    }
    let (report, _, stdout) = run_runner(&["--php", php, "--no-artifacts", "-q"], &[&dir]);
    assert!(report.totals.total >= 15, "{stdout}");
    assert_eq!(report.totals.fail + report.totals.bork, 0, "{stdout}");
    assert!(report.totals.pass >= report.totals.total - 2, "{stdout}");
}

/// A sample of long-standing Zend tests whose expected output is the same in PHP 8.3 (the
/// reference binary) and php-src master.
#[test]
fn php_src_zend_sample_passes() {
    let Some(php) = reference_php() else {
        eprintln!("skipping: {REFERENCE_PHP} not found");
        return;
    };
    let base = php_src().join("Zend/tests");
    let sample = [
        "add_001.phpt",
        "add_002.phpt",
        "and_001.phpt",
        "array_unpack/basic.phpt",
        "arrow_functions/001.phpt",
        "bug18556.phpt",
        "closures/closure_001.phpt",
        "closures/closure_002.phpt",
        "concat/concat_001.phpt",
        "enum/basic-methods.phpt",
        "exceptions/exception_001.phpt",
        "generators/auto_incrementing_keys.phpt",
        "match/001.phpt",
        "nullsafe_operator/001.phpt",
        "traits/language001.phpt",
    ];
    let paths: Vec<PathBuf> = sample
        .iter()
        .map(|s| base.join(s))
        .filter(|p| p.is_file())
        .collect();
    if paths.len() < 10 {
        eprintln!("skipping: php-src Zend/tests sample not found");
        return;
    }
    let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let (report, _, stdout) = run_runner(&["--php", php, "--no-artifacts"], &refs);
    assert_eq!(report.totals.total, paths.len(), "{stdout}");
    assert_eq!(report.totals.pass, paths.len(), "{stdout}");
}
