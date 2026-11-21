// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Integration tests: the section parser and EXPECTF matcher against fixtures, and end-to-end
//! runs of the `phpt-runner` binary with the reference `/usr/bin/php8.5` (skipped when absent).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use phpt_runner::expectf::{expectf_matches, expectf_to_regex, regex_matches};
use phpt_runner::phpt::{ExpectKind, TestFile};
use phpt_runner::report::JsonReport;
use phpt_runner::result::Status;

const REFERENCE_PHP: &str = "/usr/bin/php8.5";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run")
}

fn php_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src")
}

fn reference_php() -> Option<&'static str> {
    Path::new(REFERENCE_PHP).is_file().then_some(REFERENCE_PHP)
}

const REFERENCE_PHP_CGI: &str = "/usr/bin/php-cgi8.5";

fn reference_php_cgi() -> Option<&'static str> {
    Path::new(REFERENCE_PHP_CGI)
        .is_file()
        .then_some(REFERENCE_PHP_CGI)
}

/// Copy the fixtures into a private directory (tests write generated files next to themselves).
fn fixture_copy(tag: &str) -> PathBuf {
    copy_fixtures(&fixtures(), tag)
}

fn copy_fixtures(from: &Path, tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("phpt-fixtures-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dir.join(entry.file_name())).unwrap();
    }
    dir
}

/// Run the binary and return the parsed JSON report and exit code.
fn run_runner(args: &[&str], paths: &[&Path]) -> (JsonReport, Option<i32>, String) {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let json = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "report-{}-{}.json",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    // Runs are independent of a php-cgi beside the PHP binary unless a test asks for one.
    let no_cgi: &[&str] = if args.contains(&"--php-cgi") {
        &[]
    } else {
        &["--no-cgi"]
    };
    let output = Command::new(env!("CARGO_BIN_EXE_phpt-runner"))
        .args(args)
        .args(no_cgi)
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
            // A test killed at its time limit reports TIMEOUT (a failure).
            "fail" if name.starts_with("fail_timeout") => Status::Timeout,
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
fn scope_file_excludes_tests() {
    let Some(php) = reference_php() else {
        eprintln!("skipping: {REFERENCE_PHP} not found");
        return;
    };
    let dir =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("phpt-scope-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("outside")).unwrap();
    fs::create_dir_all(dir.join("inside")).unwrap();
    let fail = "--TEST--\nfails\n--FILE--\n<?php echo 1;\n--EXPECT--\n2\n";
    fs::write(dir.join("outside/a.phpt"), fail).unwrap();
    fs::write(dir.join("inside/b.phpt"), fail).unwrap();
    fs::write(dir.join("scope.txt"), "# test\noutside\n").unwrap();
    let baseline = dir.join("baseline.txt");
    let (report, code, stdout) = run_runner(
        &[
            "--php",
            php,
            "--srcdir",
            dir.to_str().unwrap(),
            "--scope",
            dir.join("scope.txt").to_str().unwrap(),
            "--baseline",
            baseline.to_str().unwrap(),
            "--update-baseline",
        ],
        &[&dir],
    );
    assert_eq!(code, Some(0), "{stdout}");
    assert_eq!(report.totals.total, 2, "{stdout}");
    assert_eq!(report.totals.out_of_scope, 1, "{stdout}");
    assert_eq!(report.totals.fail, 1, "{stdout}");
    let out = report
        .tests
        .iter()
        .find(|t| t.path == "outside/a.phpt")
        .unwrap();
    assert_eq!(out.result, Status::OutOfScope);
    assert_eq!(out.name, "fails");
    let text = fs::read_to_string(&baseline).unwrap();
    assert!(
        text.contains("inside/b.phpt") && !text.contains("outside"),
        "{text}"
    );

    // --no-scope runs everything.
    let (report, _, stdout) = run_runner(
        &[
            "--php",
            php,
            "--srcdir",
            dir.to_str().unwrap(),
            "--no-scope",
        ],
        &[&dir],
    );
    assert_eq!(report.totals.fail, 2, "{stdout}");
    assert_eq!(report.totals.out_of_scope, 0, "{stdout}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn environment_file_counts_host_failures_as_pass() {
    let Some(php) = reference_php() else {
        eprintln!("skipping: {REFERENCE_PHP} not found");
        return;
    };
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("phpt-environment-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("host")).unwrap();
    fs::create_dir_all(dir.join("real")).unwrap();
    let fail = "--TEST--\nfails\n--FILE--\n<?php echo 1;\n--EXPECT--\n2\n";
    fs::write(dir.join("host/env_fail.phpt"), fail).unwrap();
    fs::write(dir.join("real/bug_fail.phpt"), fail).unwrap();
    // Default discovery reads `phpt-environment.txt` from `--srcdir`.
    fs::write(
        dir.join("phpt-environment.txt"),
        "# host library limitation\nhost/env_fail.phpt\n",
    )
    .unwrap();
    let baseline = dir.join("baseline.txt");
    let (report, code, stdout) = run_runner(
        &[
            "--php",
            php,
            "--srcdir",
            dir.to_str().unwrap(),
            "--baseline",
            baseline.to_str().unwrap(),
            "--update-baseline",
        ],
        &[&dir],
    );
    assert_eq!(code, Some(0), "{stdout}");
    assert_eq!(report.totals.total, 2, "{stdout}");
    assert_eq!(report.totals.pass, 1, "{stdout}");
    assert_eq!(report.totals.fail, 1, "{stdout}");
    let env_test = report
        .tests
        .iter()
        .find(|t| t.path == "host/env_fail.phpt")
        .unwrap();
    assert_eq!(env_test.result, Status::Pass);
    assert_eq!(
        env_test.info,
        "reason: host environment (phpt-environment.txt)"
    );
    // Failure artifacts for a converted test are cleaned up.
    assert!(!dir.join("host/env_fail.diff").exists());
    assert!(!dir.join("host/env_fail.out").exists());
    assert!(!dir.join("host/env_fail.exp").exists());
    assert!(!dir.join("host/env_fail.php").exists());
    // Baseline contains only the real failure, not the environmental one.
    let text = fs::read_to_string(&baseline).unwrap();
    assert!(
        text.contains("real/bug_fail.phpt") && !text.contains("host/env_fail.phpt"),
        "{text}"
    );

    // Checking against the updated baseline succeeds.
    let (_, code, stdout) = run_runner(
        &[
            "--php",
            php,
            "--srcdir",
            dir.to_str().unwrap(),
            "--baseline",
            baseline.to_str().unwrap(),
        ],
        &[&dir],
    );
    assert_eq!(code, Some(0), "{stdout}");

    // --no-environment reports the raw failure.
    let (report, _, stdout) = run_runner(
        &[
            "--php",
            php,
            "--srcdir",
            dir.to_str().unwrap(),
            "--no-environment",
        ],
        &[&dir],
    );
    assert_eq!(report.totals.pass, 0, "{stdout}");
    assert_eq!(report.totals.fail, 2, "{stdout}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resume_log_reuses_results() {
    let Some(php) = reference_php() else {
        eprintln!("skipping: {REFERENCE_PHP} not found");
        return;
    };
    let dir = fixture_copy("resume");
    let log = dir.join("results.jsonl");
    let files: Vec<PathBuf> = ["pass_expect.phpt", "fail_expect.phpt"]
        .iter()
        .map(|f| dir.join(f))
        .collect();
    let paths: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    let args = ["--php", php, "--resume", log.to_str().unwrap()];
    let (first, _, _) = run_runner(&args, &paths[..1]);
    assert_eq!(first.totals.pass, 1);
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1);
    // The second run only executes the test that is not in the log yet.
    let (second, _, stdout) = run_runner(&args, &paths);
    assert!(stdout.contains("Resuming: 1 result(s) reused"), "{stdout}");
    assert!(stdout.contains("Running 1 test "), "{stdout}");
    assert_eq!((second.totals.pass, second.totals.fail), (1, 1), "{stdout}");
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 2);
    let _ = fs::remove_dir_all(&dir);
}

/// run-tests.php (master and 8.3 alike) evaluates SKIPIF with the base INI settings
/// (`$orig_ini_settings_args`, computed before the `--INI--` section is applied) plus `-d`
/// options, not with the test's own `--INI--` section.
#[test]
fn skipif_sees_base_ini_but_not_the_ini_section() {
    let Some(php) = reference_php() else {
        eprintln!("skipping: {REFERENCE_PHP} not found");
        return;
    };
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("phpt-skipif-ini-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("t.phpt"),
        "--TEST--\nskipif ini\n--SKIPIF--\n<?php if (ini_get('precision') !== '7') die('skip precision is ' . ini_get('precision')); ?>\n\
         --INI--\nprecision=7\n--FILE--\n<?php echo ini_get('precision');\n--EXPECT--\n7\n",
    )
    .unwrap();
    let srcdir = dir.to_str().unwrap();
    let (report, _, stdout) = run_runner(&["--php", php, "--srcdir", srcdir], &[&dir]);
    assert_eq!(report.tests[0].result, Status::Skip, "{stdout}");
    assert_eq!(report.tests[0].info, "reason: precision is 14", "{stdout}");
    let (report, _, stdout) = run_runner(
        &["--php", php, "--srcdir", srcdir, "-d", "precision=7"],
        &[&dir],
    );
    assert_eq!(report.tests[0].result, Status::Pass, "{stdout}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn env_option_reaches_tests() {
    let Some(php) = reference_php() else {
        eprintln!("skipping: {REFERENCE_PHP} not found");
        return;
    };
    let dir =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("phpt-env-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("env.phpt"),
        "--TEST--\nenv\n--FILE--\n<?php var_dump(getenv('PHPT_X'));\n--EXPECT--\nstring(3) \"a=b\"\n",
    )
    .unwrap();
    let (report, code, stdout) = run_runner(
        &[
            "--php",
            php,
            "--srcdir",
            dir.to_str().unwrap(),
            "--env",
            "PHPT_X=a=b",
        ],
        &[&dir],
    );
    assert_eq!(code, Some(0), "{stdout}");
    assert_eq!(report.totals.pass, 1, "{stdout}");
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

/// `--repeat N` (run-tests.php semantics): the script runs N times in one process, only the
/// last run's output is checked, `SKIP_REPEAT=1` is exported, and tests whose sections make
/// repeating unsafe are skipped with run-tests.php's reasons.
#[test]
fn repeat_mode_against_reference_php() {
    let Some(php) = reference_php() else {
        eprintln!("skipping: {REFERENCE_PHP} not found");
        return;
    };
    let dir = fixture_copy("repeat");
    fs::write(
        dir.join("pass_repeat_state.phpt"),
        "--TEST--\nrepeat state\n--SKIPIF--\n<?php if (getenv('SKIP_REPEAT') !== '1') die('skip only with --repeat'); ?>\n\
         --FILE--\n<?php\nfunction counter() { static $n = 0; return ++$n; }\necho counter(), counter(), \"\\n\";\n\
         --EXPECT--\n12\n",
    )
    .unwrap();
    let (report, _, stdout) = run_runner(
        &[
            "--php",
            php,
            "--timeout",
            "2",
            "-j",
            "4",
            "--no-color",
            "--repeat",
            "3",
        ],
        &[&dir],
    );
    let got: BTreeMap<String, (Status, String)> = report
        .tests
        .iter()
        .map(|t| {
            (
                t.path.rsplit('/').next().unwrap().to_string(),
                (t.result, t.info.clone()),
            )
        })
        .collect();
    let not_repeatable = [
        ("bork_clean.phpt", "CLEAN"),
        ("pass_clean.phpt", "CLEAN"),
        ("pass_stdin.phpt", "STDIN"),
        ("pass_external.phpt", "FILE_EXTERNAL"),
    ];
    for (name, section) in not_repeatable {
        assert_eq!(
            got[name],
            (
                Status::Skip,
                format!("reason: Test with {section} might not be repeatable")
            ),
            "{stdout}"
        );
    }
    assert_eq!(
        got["skip_cgi.phpt"],
        (Status::Skip, "reason: CGI not available".to_string()),
        "{stdout}"
    );
    for (name, (status, _)) in &got {
        if not_repeatable.iter().any(|(n, _)| n == name) {
            continue;
        }
        let expected = match name.split('_').next().unwrap() {
            "pass" => Status::Pass,
            // A test killed at its time limit reports TIMEOUT (a failure).
            "fail" if name.starts_with("fail_timeout") => Status::Timeout,
            "fail" => Status::Fail,
            "skip" => Status::Skip,
            "xfail" => Status::Xfail,
            "warn" => Status::Warn,
            "bork" => Status::Bork,
            other => panic!("unexpected fixture prefix {other}"),
        };
        assert_eq!(*status, expected, "{name}\n{stdout}");
    }
    assert_eq!(got.len(), 24, "{stdout}");
    // Without --repeat the state test skips itself.
    let (report, _, stdout) = run_runner(
        &["--php", php, "--no-color"],
        &[&dir.join("pass_repeat_state.phpt")],
    );
    assert_eq!(report.tests[0].result, Status::Skip, "{stdout}");
    let _ = fs::remove_dir_all(&dir);
}

/// `--junit FILE` writes run-tests.php's JUnit XML.
#[test]
fn junit_report_against_reference_php() {
    let Some(php) = reference_php() else {
        eprintln!("skipping: {REFERENCE_PHP} not found");
        return;
    };
    let dir = fixture_copy("junit");
    let xml_path = dir.join("junit.xml");
    let (_, _, stdout) = run_runner(
        &[
            "--php",
            php,
            "--timeout",
            "2",
            "-j",
            "4",
            "--no-color",
            "--srcdir",
            dir.to_str().unwrap(),
            "--junit",
            xml_path.to_str().unwrap(),
        ],
        &[&dir],
    );
    let xml = fs::read_to_string(&xml_path).unwrap_or_else(|e| panic!("{e}\n{stdout}"));
    assert!(
        xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites name=\"php\" tests=\"23\" failures=\"2\" errors=\"3\" skip=\"3\" time=\""),
        "{xml}"
    );
    assert!(xml.ends_with("</testsuite>\n</testsuites>"), "{xml}");
    assert_eq!(xml.matches("<testcase ").count(), 23, "{xml}");
    for needle in [
        "<testcase name='fail_expect.phpt (A failing test)' time='",
        "<failure type='FAILED' message=''>001- expected\n001+ actual\n</failure>\n</testcase>\n",
        // SKIPIF output and CLEAN output are the BORK messages.
        "<testcase name='bork_clean.phpt (CLEAN output borks a passing test)' time='",
        "<testcase name='bork_skipif_output.phpt (SKIPIF printing garbage)' time='",
        "<error type='BORKED' message='garbage'/>\n</testcase>\n",
        // A test that cannot be parsed is named after its file.
        "<testcase name='bork_unknown_section.phpt (bork_unknown_section.phpt)' time='",
        "<error type='BORKED' message='Unknown section &quot;NOPE&quot;'/>\n",
        "<skipped>not today</skipped>\n",
        "<skipped>Required extension missing: definitely_not_an_extension</skipped>\n",
        "<warning> (warn: XFAIL section but test passes)</warning>\n",
    ] {
        assert!(xml.contains(needle), "missing {needle:?} in\n{xml}");
    }
    assert_eq!(xml.matches("message='garbage'").count(), 2, "{xml}");

    // Inside php-src, suites are named after the test directory.
    let zend = php_src().join("Zend/tests/add_001.phpt");
    if zend.is_file() {
        let (_, _, stdout) = run_runner(
            &[
                "--php",
                php,
                "--no-artifacts",
                "--junit",
                xml_path.to_str().unwrap(),
            ],
            &[&zend],
        );
        let xml = fs::read_to_string(&xml_path).unwrap_or_else(|e| panic!("{e}\n{stdout}"));
        assert!(
            xml.contains("<testsuite name=\"php.Zend.tests\" tests=\"1\" failures=\"0\" errors=\"0\" skip=\"0\" time=\""),
            "{xml}"
        );
        assert!(
            xml.contains("<testcase name='Zend/tests/add_001.phpt (adding arrays)' time='"),
            "{xml}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

/// CGI tests (`--POST--`, `--GET--`, `--COOKIE--`, `--POST_RAW--`, `--EXPECTHEADERS--`) run
/// through the reference `php-cgi -C`, with response headers split off the output.
#[test]
fn cgi_fixtures_against_reference_php_cgi() {
    let (Some(php), Some(php_cgi)) = (reference_php(), reference_php_cgi()) else {
        eprintln!("skipping: {REFERENCE_PHP} or {REFERENCE_PHP_CGI} not found");
        return;
    };
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cgi");
    let dir = copy_fixtures(&src, "cgi");
    let (report, _, stdout) = run_runner(
        &[
            "--php",
            php,
            "--php-cgi",
            php_cgi,
            "--no-color",
            "--show-diff",
            "--srcdir",
            dir.to_str().unwrap(),
        ],
        &[&dir],
    );
    let got: BTreeMap<String, Status> = report
        .tests
        .iter()
        .map(|t| (t.path.clone(), t.result))
        .collect();
    assert_eq!(got.len(), 6, "{stdout}");
    for (name, status) in &got {
        let expected = if name.starts_with("fail_") {
            Status::Fail
        } else {
            Status::Pass
        };
        assert_eq!(*status, expected, "{name}\n{stdout}");
    }
    let diff = fs::read_to_string(dir.join("fail_headers.diff")).unwrap();
    assert!(diff.contains("--HEADERS--"), "{diff}");
    assert!(diff.contains("- X-Test: 2"), "{diff}");
    assert!(diff.contains("+ X-Test: 1"), "{diff}");
    // Without a CGI binary (and with --repeat) they skip with run-tests.php's reasons.
    let (report, _, stdout) =
        run_runner(&["--php", php, "--srcdir", dir.to_str().unwrap()], &[&dir]);
    assert!(
        report
            .tests
            .iter()
            .all(|t| t.result == Status::Skip && t.info == "reason: CGI not available"),
        "{stdout}"
    );
    let (report, _, stdout) = run_runner(
        &[
            "--php",
            php,
            "--php-cgi",
            php_cgi,
            "--repeat",
            "2",
            "--srcdir",
            dir.to_str().unwrap(),
        ],
        &[&dir],
    );
    assert!(
        report
            .tests
            .iter()
            .all(|t| t.result == Status::Skip && t.info == "reason: CGI does not support --repeat"),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}
