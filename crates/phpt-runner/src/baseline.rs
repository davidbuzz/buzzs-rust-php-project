//! Known-failure baselines: a text file listing one test path per line (`#` starts a comment).

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use crate::result::TestResult;

/// A set of test paths that are known to fail.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Baseline {
    entries: BTreeSet<String>,
}

/// How a run compares to the baseline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comparison {
    /// Tests that failed but are not in the baseline.
    pub new_failures: Vec<String>,
    /// Baseline tests that now pass (the baseline can shrink).
    pub unexpected_passes: Vec<String>,
}

impl Comparison {
    /// True if CI should fail.
    pub fn is_regression(&self) -> bool {
        !self.new_failures.is_empty() || !self.unexpected_passes.is_empty()
    }
}

impl Baseline {
    /// Parse baseline text.
    pub fn parse(text: &str) -> Self {
        let entries = text
            .lines()
            .map(|l| l.split('#').next().unwrap_or_default().trim())
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        Baseline { entries }
    }

    /// Load a baseline; a missing file is an empty baseline.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Whether `path` is a known failure.
    pub fn contains(&self, path: &str) -> bool {
        self.entries.contains(path)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if the baseline lists nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Compare a run against the baseline. Skipped and XFAIL tests are neutral.
    pub fn compare(&self, results: &[TestResult]) -> Comparison {
        let mut cmp = Comparison::default();
        for r in results {
            let known = self.contains(&r.path);
            if r.result.is_failure() && !known {
                cmp.new_failures.push(r.path.clone());
            } else if r.result.is_pass() && known {
                cmp.unexpected_passes.push(r.path.clone());
            }
        }
        cmp.new_failures.sort();
        cmp.unexpected_passes.sort();
        cmp
    }

    /// The baseline after this run: entries for tests that were not run are kept, entries for
    /// tests that ran are replaced by the current failures.
    pub fn updated(&self, results: &[TestResult]) -> Baseline {
        let ran: BTreeSet<&str> = results.iter().map(|r| r.path.as_str()).collect();
        let mut entries: BTreeSet<String> = self
            .entries
            .iter()
            .filter(|e| !ran.contains(e.as_str()))
            .cloned()
            .collect();
        entries.extend(
            results
                .iter()
                .filter(|r| r.result.is_failure())
                .map(|r| r.path.clone()),
        );
        Baseline { entries }
    }

    /// Serialize (sorted, one path per line, with a header comment).
    pub fn to_text(&self) -> String {
        let mut s = String::from(
            "# phpt-runner baseline: tests known to FAIL or BORK. Regenerate with --update-baseline.\n",
        );
        for e in &self.entries {
            s.push_str(e);
            s.push('\n');
        }
        s
    }

    /// Write the baseline to `path`.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        fs::write(path, self.to_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::Status;

    fn r(path: &str, result: Status) -> TestResult {
        TestResult {
            path: path.into(),
            name: path.into(),
            result,
            info: String::new(),
            duration_ms: 0,
            diff: None,
        }
    }

    #[test]
    fn compare_and_update() {
        let b = Baseline::parse("# c\na.phpt\nb.phpt # trailing\n\nz.phpt\n");
        assert_eq!(b.len(), 3);
        let results = vec![
            r("a.phpt", Status::Fail),
            r("b.phpt", Status::Pass),
            r("c.phpt", Status::Bork),
            r("d.phpt", Status::Skip),
        ];
        let cmp = b.compare(&results);
        assert_eq!(cmp.new_failures, vec!["c.phpt"]);
        assert_eq!(cmp.unexpected_passes, vec!["b.phpt"]);
        assert!(cmp.is_regression());
        let u = b.updated(&results);
        assert_eq!(u.to_text().lines().skip(1).collect::<Vec<_>>(), ["a.phpt", "c.phpt", "z.phpt"]);
    }
}
