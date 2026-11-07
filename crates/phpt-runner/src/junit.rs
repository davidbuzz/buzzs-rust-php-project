//! `JUnit` XML report in run-tests.php's format (ref: `run-tests.php` class `JUnit`, enabled
//! there by `TEST_PHP_JUNIT`).
//!
//! The layout, attribute quoting, counters, suite names (`pathToClassName()`), time formats and
//! escaping follow run-tests.php. Deliberate differences:
//! - Every counted test gets a `<testcase>` element. run-tests.php counts tests skipped before
//!   their SKIPIF ran (`skip_test()`: CGI/phpdbg not available, missing extensions, ...) and
//!   tests that fail to parse, but never starts their timer, so their element is missing.
//! - Test cases are ordered by path (run-tests.php: completion order per worker), so reports
//!   of the same run are reproducible.
//! - Times come from the runner's millisecond durations (and are summed exactly).
//! - Out-of-scope tests (`phpt-scope.txt`) are not run and not reported.
//! - Messages and diffs are valid UTF-8 (lossily converted earlier), where `htmlspecialchars()`
//!   would turn a string with invalid UTF-8 into the empty string.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::result::{Status, TestResult};

/// Name of the root suite.
const ROOT: &str = "php";

/// Counters of one suite (`JUnit::EMPTY_SUITE`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Tally {
    total: usize,
    pass: usize,
    fail: usize,
    error: usize,
    skip: usize,
    warn: usize,
    time_ms: u64,
}

impl Tally {
    fn add(&mut self, other: &Tally) {
        self.total += other.total;
        self.pass += other.pass;
        self.fail += other.fail;
        self.error += other.error;
        self.skip += other.skip;
        self.warn += other.warn;
        self.time_ms += other.time_ms;
    }
}

/// One suite: its counters and the XML of its test cases.
#[derive(Debug, Default)]
struct Suite {
    tally: Tally,
    cases: String,
}

/// The whole report for `results` (paths relative to `srcdir`, the php-src root).
pub fn junit_xml(results: &[TestResult], srcdir: &Path) -> String {
    let mut sorted: Vec<&TestResult> = results
        .iter()
        .filter(|r| r.result != Status::OutOfScope)
        .collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));

    let mut suites: Vec<(String, Suite)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut names: HashMap<String, String> = HashMap::new();
    let mut root = Tally::default();
    for r in sorted {
        let dir = php_dirname(&r.path);
        let name = names
            .entry(dir.clone())
            .or_insert_with(|| suite_name(&dir, srcdir))
            .clone();
        let i = *index.entry(name.clone()).or_insert_with(|| {
            suites.push((name, Suite::default()));
            suites.len() - 1
        });
        let suite = &mut suites[i].1;
        let tally = mark_test(&mut suite.cases, r);
        suite.tally.add(&tally);
        root.add(&tally);
    }

    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    push_suite_open(&mut xml, "testsuites", ROOT, &root);
    for (name, suite) in &suites {
        push_suite_open(&mut xml, "testsuite", name, &suite.tally);
        xml.push_str(&suite.cases);
        xml.push_str("</testsuite>\n");
    }
    xml.push_str("</testsuites>");
    xml
}

fn push_suite_open(xml: &mut String, element: &str, name: &str, t: &Tally) {
    let _ = writeln!(
        xml,
        "<{element} name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"{}\" skip=\"{}\" time=\"{}\">",
        htmlspecialchars(name),
        t.total,
        t.fail,
        t.error,
        t.skip,
        float_seconds(t.time_ms)
    );
}

/// `JUnit::markTestAs()`: append the test case element to `cases` and return its counts.
fn mark_test(cases: &mut String, r: &TestResult) -> Tally {
    let mut t = Tally {
        total: 1,
        time_ms: r.duration_ms,
        ..Tally::default()
    };
    let title = r.title.as_deref().unwrap_or(&r.name);
    let _ = writeln!(
        cases,
        "<testcase name='{}' time='{}'>",
        htmlspecialchars(&format!("{} ({title})", r.path)),
        number_format_4(r.duration_ms)
    );
    let msg = htmlspecialchars(message(r));
    match r.result {
        Status::Pass | Status::Xfail => t.pass = 1,
        Status::Bork => {
            t.error = 1;
            let _ = writeln!(cases, "<error type='BORKED' message='{msg}'/>");
        }
        Status::Skip => {
            t.skip = 1;
            let _ = writeln!(cases, "<skipped>{msg}</skipped>");
        }
        Status::Warn => {
            t.warn = 1;
            let _ = writeln!(cases, "<warning>{msg}</warning>");
        }
        Status::Fail => {
            t.fail = 1;
            // A failing test whose SKIPIF warned has the result types WARN&FAIL; the element
            // is a failure typed after the first one.
            let kind = if r.info.starts_with(" (warn: ") {
                "WARNED"
            } else {
                "FAILED"
            };
            let details = r.diff.as_deref().map_or_else(String::new, escape_details);
            let _ = writeln!(
                cases,
                "<failure type='{kind}' message='{msg}'>{details}</failure>"
            );
        }
        Status::Timeout => {
            t.fail = 1;
            let details = r.diff.as_deref().map_or_else(String::new, escape_details);
            let _ = writeln!(
                cases,
                "<failure type='TIMEOUT' message='{msg}'>{details}</failure>"
            );
        }
        Status::OutOfScope => {}
    }
    cases.push_str("</testcase>\n");
    t
}

/// The message run-tests.php passes to `markTestAs()` for this result.
fn message(r: &TestResult) -> &str {
    if let Some(m) = &r.junit_message {
        return m;
    }
    match r.result {
        Status::Skip => r.info.strip_prefix("reason: ").unwrap_or(&r.info),
        // The result line of a BORK carries its message in the name position.
        Status::Bork => &r.name,
        _ => &r.info,
    }
}

/// The failure details: the diff with ESC shown as `<esc>`, escaped, and the remaining
/// control characters XML 1.0 cannot carry written as `[[0xNN]]`.
fn escape_details(diff: &str) -> String {
    let escaped = htmlspecialchars(&diff.replace('\x1b', "<esc>"));
    let mut out = String::with_capacity(escaped.len());
    for c in escaped.chars() {
        if matches!(c, '\0'..='\x08' | '\x0b' | '\x0c' | '\x0e'..='\x1f') {
            let _ = write!(out, "[[0x{:02x}]]", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

/// `htmlspecialchars($s, ENT_QUOTES)`.
fn htmlspecialchars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#039;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// `number_format($seconds, 4, '.', '')` of a millisecond duration.
fn number_format_4(ms: u64) -> String {
    format!("{}.{:03}0", ms / 1000, ms % 1000)
}

/// PHP's string conversion (`precision=14`, `%s` in run-tests.php's `sprintf()`) of a float
/// number of seconds, for a millisecond count: `0`, `12`, `1.5`, `0.034`. Millisecond values
/// never need more than 14 significant digits or the exponent form.
fn float_seconds(ms: u64) -> String {
    let (secs, frac) = (ms / 1000, ms % 1000);
    if frac == 0 {
        return secs.to_string();
    }
    let digits = format!("{frac:03}");
    format!("{secs}.{}", digits.trim_end_matches('0'))
}

/// PHP `dirname()` of a `/`-separated path: `a/b/c.phpt` -> `a/b`, `c.phpt` -> `.`,
/// `/c.phpt` -> `/`.
fn php_dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return if path.is_empty() {
            ".".into()
        } else {
            "/".into()
        };
    }
    match trimmed.rfind('/') {
        None => ".".into(),
        Some(i) => {
            let dir = trimmed[..i].trim_end_matches('/');
            if dir.is_empty() {
                "/".into()
            } else {
                dir.into()
            }
        }
    }
}

/// `JUnit::pathToClassName()` of a test directory (relative to `srcdir`, the working
/// directory of the tests): walk up at most five levels looking for the php-src root (the
/// directory with `main/php_version.h`); below it, the path components joined by `.` with
/// runs of characters other than ASCII letters and digits replaced by `.`. Otherwise the
/// directory with `/` and `-` replaced by `.`.
pub fn suite_name(dir: &str, srcdir: &Path) -> String {
    let joined = srcdir.join(dir);
    let mut cur: PathBuf = joined.canonicalize().unwrap_or(joined);
    let mut parts: VecDeque<String> = VecDeque::new();
    let mut max = 5;
    let found = loop {
        parts.push_front(
            cur.file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        );
        let chk = cur.join("main").join("php_version.h");
        if let Some(parent) = cur.parent().map(Path::to_path_buf) {
            cur = parent;
        }
        if chk.exists() {
            break true;
        }
        max -= 1;
        if max == 0 {
            break false;
        }
    };
    if !found {
        return format!("{ROOT}.{}", dir.replace(['/', '-'], "."));
    }
    parts.pop_front();
    let mut name = String::from(ROOT);
    for part in parts {
        name.push('.');
        name.push_str(&class_component(&part));
    }
    name
}

/// `preg_replace(",[^a-z0-9]+,i", ".", $part)`.
fn class_component(part: &str) -> String {
    let mut out = String::with_capacity(part.len());
    let mut in_run = false;
    for c in part.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('.');
            in_run = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn result(path: &str, status: Status, info: &str) -> TestResult {
        TestResult {
            path: path.into(),
            name: "Title".into(),
            result: status,
            info: info.into(),
            duration_ms: 12,
            diff: None,
            title: None,
            junit_message: None,
        }
    }

    /// A fake php-src root with `main/php_version.h`.
    fn fake_srcdir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("phpt-runner-junit-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for sub in ["main", "Zend/tests", "ext/foo-bar/tests", "a/b/c/d/e-f"] {
            fs::create_dir_all(dir.join(sub)).expect("create fixture dir");
        }
        fs::write(dir.join("main/php_version.h"), "").expect("write fixture");
        dir
    }

    #[test]
    fn suite_names_follow_path_to_class_name() {
        let src = fake_srcdir("names");
        assert_eq!(suite_name("Zend/tests", &src), "php.Zend.tests");
        assert_eq!(
            suite_name("ext/foo-bar/tests", &src),
            "php.ext.foo.bar.tests"
        );
        assert_eq!(suite_name(".", &src), "php");
        assert_eq!(suite_name("a/b/c/d", &src), "php.a.b.c.d");
        // Five levels below the root: the walk gives up before reaching it.
        assert_eq!(suite_name("a/b/c/d/e-f", &src), "php.a.b.c.d.e.f");
        let _ = fs::remove_dir_all(&src);
    }

    #[test]
    fn helpers() {
        assert_eq!(php_dirname("Zend/tests/a.phpt"), "Zend/tests");
        assert_eq!(php_dirname("a.phpt"), ".");
        assert_eq!(php_dirname("/a.phpt"), "/");
        assert_eq!(class_component("foo--bar_baz"), "foo.bar.baz");
        assert_eq!(number_format_4(0), "0.0000");
        assert_eq!(number_format_4(12_345), "12.3450");
        assert_eq!(float_seconds(0), "0");
        assert_eq!(float_seconds(2000), "2");
        assert_eq!(float_seconds(1500), "1.5");
        assert_eq!(float_seconds(34), "0.034");
        assert_eq!(htmlspecialchars("a&b<'\">"), "a&amp;b&lt;&#039;&quot;&gt;");
        assert_eq!(
            escape_details("x\x1by\x01\t\n"),
            "x&lt;esc&gt;y[[0x01]]\t\n"
        );
    }

    #[test]
    fn report_layout() {
        let src = fake_srcdir("layout");
        let mut fail = result("Zend/tests/b.phpt", Status::Fail, "");
        fail.diff = Some("001+ got 'x'\n001- want\n".into());
        let mut bork = result("Zend/tests/c.phpt", Status::Bork, "reason: invalid output");
        bork.name = "garbage".into();
        bork.title = Some("C title".into());
        let results = vec![
            result("ext/foo-bar/tests/z.phpt", Status::Skip, "reason: no foo"),
            fail,
            result("Zend/tests/a.phpt", Status::Pass, ""),
            bork,
            result(
                "Zend/tests/d.phpt",
                Status::Warn,
                " (warn: XFAIL section but test passes)",
            ),
            result("Zend/tests/e.phpt", Status::Xfail, "  XFAIL REASON: later"),
            result(
                "Zend/tests/f.phpt",
                Status::OutOfScope,
                "reason: out of scope",
            ),
        ];
        let xml = junit_xml(&results, &src);
        let expected = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<testsuites name=\"php\" tests=\"6\" failures=\"1\" errors=\"1\" skip=\"1\" time=\"0.072\">\n\
<testsuite name=\"php.Zend.tests\" tests=\"5\" failures=\"1\" errors=\"1\" skip=\"0\" time=\"0.06\">\n\
<testcase name='Zend/tests/a.phpt (Title)' time='0.0120'>\n\
</testcase>\n\
<testcase name='Zend/tests/b.phpt (Title)' time='0.0120'>\n\
<failure type='FAILED' message=''>001+ got &#039;x&#039;\n001- want\n</failure>\n\
</testcase>\n\
<testcase name='Zend/tests/c.phpt (C title)' time='0.0120'>\n\
<error type='BORKED' message='garbage'/>\n\
</testcase>\n\
<testcase name='Zend/tests/d.phpt (Title)' time='0.0120'>\n\
<warning> (warn: XFAIL section but test passes)</warning>\n\
</testcase>\n\
<testcase name='Zend/tests/e.phpt (Title)' time='0.0120'>\n\
</testcase>\n\
</testsuite>\n\
<testsuite name=\"php.ext.foo.bar.tests\" tests=\"1\" failures=\"0\" errors=\"0\" skip=\"1\" time=\"0.012\">\n\
<testcase name='ext/foo-bar/tests/z.phpt (Title)' time='0.0120'>\n\
<skipped>no foo</skipped>\n\
</testcase>\n\
</testsuite>\n\
</testsuites>";
        assert_eq!(xml, expected);
        let _ = fs::remove_dir_all(&src);
    }
}
