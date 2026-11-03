//! Summaries: per-directory table, run-tests.php style totals and JSON output.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::result::{Status, TestResult};

/// Result counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    /// All tests.
    pub total: usize,
    /// PASS (including WARN).
    pub pass: usize,
    /// FAIL.
    pub fail: usize,
    /// SKIP.
    pub skip: usize,
    /// XFAIL.
    pub xfail: usize,
    /// BORK.
    pub bork: usize,
    /// WARN (also included in `pass`).
    pub warn: usize,
    /// Out of scope (not run; excluded from the pass rates).
    #[serde(default)]
    pub out_of_scope: usize,
}

impl Counts {
    /// Add one result.
    pub fn add(&mut self, status: Status) {
        self.total += 1;
        match status {
            Status::Pass => self.pass += 1,
            Status::Warn => {
                self.pass += 1;
                self.warn += 1;
            }
            Status::Fail => self.fail += 1,
            Status::Xfail => self.xfail += 1,
            Status::Skip => self.skip += 1,
            Status::Bork => self.bork += 1,
            Status::OutOfScope => self.out_of_scope += 1,
        }
    }

    /// In-scope tests.
    pub fn in_scope(&self) -> usize {
        self.total - self.out_of_scope
    }

    /// In-scope tests that actually ran (not skipped or borked).
    pub fn ran(&self) -> usize {
        self.in_scope() - self.skip - self.bork
    }

    /// Pass percentage of the tests that ran.
    pub fn pass_rate(&self) -> f64 {
        percent(self.pass, self.ran())
    }

    /// Pass percentage of all in-scope tests (skips count against).
    pub fn pass_rate_all(&self) -> f64 {
        percent(self.pass, self.in_scope())
    }
}

#[allow(clippy::cast_precision_loss)]
fn percent(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        100.0 * n as f64 / d as f64
    }
}

/// Directory key for grouping: the parent directory of `path`, truncated to `depth` components
/// if given.
pub fn dir_key(path: &str, depth: Option<usize>) -> String {
    let parent = path.rsplit_once('/').map_or("", |(d, _)| d);
    match depth {
        Some(n) if n > 0 => parent.split('/').take(n).collect::<Vec<_>>().join("/"),
        _ => parent.to_string(),
    }
}

/// Group results by directory.
pub fn by_dir(results: &[TestResult], depth: Option<usize>) -> BTreeMap<String, Counts> {
    let mut map: BTreeMap<String, Counts> = BTreeMap::new();
    for r in results {
        map.entry(dir_key(&r.path, depth))
            .or_default()
            .add(r.result);
    }
    map
}

/// Totals over all results.
pub fn totals(results: &[TestResult]) -> Counts {
    let mut c = Counts::default();
    for r in results {
        c.add(r.result);
    }
    c
}

/// The per-directory table.
pub fn dir_table(dirs: &BTreeMap<String, Counts>, total: &Counts) -> String {
    let width = dirs.keys().map(String::len).max().unwrap_or(0).max(9);
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{:<width$} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>7}",
        "Directory", "total", "pass", "fail", "skip", "xfail", "bork", "out", "pass%"
    );
    let _ = writeln!(s, "{}", "-".repeat(width + 57));
    let row = |s: &mut String, name: &str, c: &Counts| {
        let _ = writeln!(
            s,
            "{name:<width$} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6.1}%",
            c.total,
            c.pass,
            c.fail,
            c.skip,
            c.xfail,
            c.bork,
            c.out_of_scope,
            c.pass_rate()
        );
    };
    for (dir, c) in dirs {
        row(&mut s, dir, c);
    }
    let _ = writeln!(s, "{}", "-".repeat(width + 57));
    row(&mut s, "TOTAL", total);
    s
}

/// run-tests.php style summary block (over in-scope tests).
pub fn summary(total: &Counts, seconds: f64) -> String {
    let n = total.in_scope();
    let x = total.ran();
    let pct = |v: usize| percent(v, n);
    let xpct = |v: usize| percent(v, x);
    let mut s = String::new();
    s.push_str("=====================================================================\n");
    let _ = writeln!(s, "Number of tests : {n:5}          {x:8}");
    if total.out_of_scope > 0 {
        let _ = writeln!(
            s,
            "Out of scope    : {:5} (not run, not counted)",
            total.out_of_scope
        );
    }
    if total.bork > 0 {
        let _ = writeln!(
            s,
            "Tests borked    : {:5} ({:5.1}%) --------",
            total.bork,
            pct(total.bork)
        );
    }
    let _ = writeln!(
        s,
        "Tests skipped   : {:5} ({:5.1}%) --------",
        total.skip,
        pct(total.skip)
    );
    let _ = writeln!(
        s,
        "Tests warned    : {:5} ({:5.1}%) ({:5.1}%)",
        total.warn,
        pct(total.warn),
        xpct(total.warn)
    );
    let _ = writeln!(
        s,
        "Tests failed    : {:5} ({:5.1}%) ({:5.1}%)",
        total.fail,
        pct(total.fail),
        xpct(total.fail)
    );
    if total.xfail > 0 {
        let _ = writeln!(
            s,
            "Expected fail   : {:5} ({:5.1}%) ({:5.1}%)",
            total.xfail,
            pct(total.xfail),
            xpct(total.xfail)
        );
    }
    let passed = total.pass - total.warn;
    let _ = writeln!(
        s,
        "Tests passed    : {:5} ({:5.1}%) ({:5.1}%)",
        passed,
        pct(passed),
        xpct(passed)
    );
    s.push_str("---------------------------------------------------------------------\n");
    let _ = writeln!(s, "Time taken      : {seconds:5.3} seconds");
    s.push_str("=====================================================================\n");
    s
}

/// Per-directory entry in the JSON report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirReport {
    /// Directory relative to the php-src root.
    pub dir: String,
    /// Counts.
    #[serde(flatten)]
    pub counts: Counts,
}

/// The `--json` report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonReport {
    /// The PHP binary tested.
    pub php: String,
    /// The php-src root.
    pub srcdir: String,
    /// Totals.
    pub totals: Counts,
    /// Per-directory counts (full parent directories).
    pub dirs: Vec<DirReport>,
    /// Every test result.
    pub tests: Vec<TestResult>,
}

impl JsonReport {
    /// Build a report.
    pub fn new(php: String, srcdir: String, results: &[TestResult]) -> Self {
        let mut tests = results.to_vec();
        tests.sort_by(|a, b| a.path.cmp(&b.path));
        JsonReport {
            php,
            srcdir,
            totals: totals(results),
            dirs: by_dir(results, None)
                .into_iter()
                .map(|(dir, counts)| DirReport { dir, counts })
                .collect(),
            tests,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping() {
        assert_eq!(dir_key("Zend/tests/foo/a.phpt", None), "Zend/tests/foo");
        assert_eq!(dir_key("Zend/tests/foo/a.phpt", Some(2)), "Zend/tests");
        assert_eq!(dir_key("a.phpt", None), "");
    }

    #[test]
    fn counts_and_rates() {
        let mut c = Counts::default();
        for s in [
            Status::Pass,
            Status::Warn,
            Status::Fail,
            Status::Skip,
            Status::OutOfScope,
        ] {
            c.add(s);
        }
        assert_eq!(c.pass, 2);
        assert_eq!(c.ran(), 3);
        assert!((c.pass_rate() - 66.666).abs() < 0.01);
        assert!((c.pass_rate_all() - 50.0).abs() < 0.01);
        assert_eq!(c.out_of_scope, 1);
        assert!(summary(&c, 1.0).contains("Out of scope    :     1"));
        let table = dir_table(&BTreeMap::from([("x".to_string(), c)]), &c);
        assert!(table.contains("TOTAL"));
        assert!(summary(&c, 1.0).contains("Tests passed    :     1"));
    }
}
