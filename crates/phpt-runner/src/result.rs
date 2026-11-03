//! Test outcomes.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The outcome of a single test, named as in run-tests.php output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Status {
    /// Output matched.
    Pass,
    /// Output matched but with a warning (XFAIL section present, passed on retry, SKIPIF warn).
    Warn,
    /// Output did not match.
    Fail,
    /// Output did not match, as the `--XFAIL--` section predicts.
    Xfail,
    /// Skipped by SKIPIF, a missing extension or an unsupported SAPI.
    Skip,
    /// The test file itself is broken or its SKIPIF/CLEAN printed garbage.
    Bork,
}

impl Status {
    /// The label printed at the start of a result line.
    pub fn label(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
            Status::Xfail => "XFAIL",
            Status::Skip => "SKIP",
            Status::Bork => "BORK",
        }
    }

    /// Counts as passing for pass-rate purposes.
    pub fn is_pass(self) -> bool {
        matches!(self, Status::Pass | Status::Warn)
    }

    /// Counts as a failure for CI / baseline purposes.
    pub fn is_failure(self) -> bool {
        matches!(self, Status::Fail | Status::Bork)
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// The result of one test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestResult {
    /// Path relative to the php-src root, e.g. `Zend/tests/foo.phpt`.
    pub path: String,
    /// The `--TEST--` title (or the bork message).
    pub name: String,
    /// Outcome.
    pub result: Status,
    /// Skip reason, XFAIL reason, warning text...
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub info: String,
    /// Wall-clock time in milliseconds.
    #[serde(default)]
    pub duration_ms: u64,
    /// The diff for failures (not serialized; written to `.diff` files instead).
    #[serde(skip)]
    pub diff: Option<String>,
}

impl TestResult {
    /// The run-tests.php style result line, e.g. `FAIL Foo test [Zend/tests/foo.phpt] `.
    pub fn line(&self) -> String {
        format!("{} {} [{}] {}", self.result, self.name, self.path, self.info)
    }
}
