//! Host-environment failure overrides: tests whose failures are caused only by host library or
//! service limitations, where the system C PHP binaries fail identically.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::result::{Status, TestResult};

/// File name looked up in the ancestors of the php-src root when `--environment` is not given.
pub const DEFAULT_ENVIRONMENT_FILE: &str = "phpt-environment.txt";

/// Host-environment failure overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    paths: HashSet<String>,
}

impl Environment {
    /// Parse environment-file text: one test path per line, `#` starts a comment.
    pub fn parse(text: &str) -> Self {
        let paths = text
            .lines()
            .map(|l| l.split('#').next().unwrap_or_default().trim())
            .filter(|l| !l.is_empty())
            .map(|l| l.trim_start_matches("./").to_string())
            .collect();
        Environment { paths }
    }

    /// Load an environment file.
    pub fn load(path: &Path) -> io::Result<Self> {
        Ok(Self::parse(&fs::read_to_string(path)?))
    }

    /// Find [`DEFAULT_ENVIRONMENT_FILE`] in `start` or one of its ancestors.
    pub fn find_default(start: &Path) -> Option<PathBuf> {
        start
            .ancestors()
            .map(|a| a.join(DEFAULT_ENVIRONMENT_FILE))
            .find(|p| p.is_file())
    }

    /// Does this environment file contain `path`?
    pub fn contains(&self, path: &str) -> bool {
        self.paths.contains(path.trim_start_matches("./"))
    }

    /// Convert a failure of a listed test into a `PASS`.
    pub fn apply_one(&self, result: &mut TestResult, file_name: &str) -> bool {
        if result.result == Status::Fail && self.contains(&result.path) {
            result.result = Status::Pass;
            result.info = format!("reason: host environment ({file_name})");
            result.diff = None;
            true
        } else {
            false
        }
    }

    /// Convert failures of listed tests across `results` into `PASS`.
    pub fn apply(&self, results: &mut [TestResult], file_name: &str) {
        for r in results {
            self.apply_one(r, file_name);
        }
    }
}
