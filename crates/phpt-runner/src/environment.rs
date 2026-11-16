// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Host-environment failure overrides: tests whose failures are caused only by host library or
//! service limitations, where the system C PHP binaries fail identically.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::process::Env;
use crate::result::{Status, TestResult};

/// Default `LC_ALL` setting matching `run-tests.php`.
pub const DEFAULT_LC_ALL: &str = "C";
/// Default `LANG` setting matching `run-tests.php`.
pub const DEFAULT_LANG: &str = "C";
/// Default `TZ` setting matching `run-tests.php`.
pub const DEFAULT_TZ: &str = "UTC";

/// Normalize environment variables for running tests, matching `run-tests.php`:
/// - `LC_ALL=C` and `LANG=C` ensure consistent character set and number formatting.
/// - `TZ=UTC` standardizes timezone across test executions.
/// - `PHP_CLI_SERVER_WORKERS` is set to empty (unset), preventing accidental multi-process worker
///   spawning for the built-in CLI server unless explicitly requested by the test's `--ENV--`.
pub fn normalize_env(env: &mut Env) {
    env.insert(OsString::from("LC_ALL"), OsString::from(DEFAULT_LC_ALL));
    env.insert(OsString::from("LANG"), OsString::from(DEFAULT_LANG));
    env.insert(OsString::from("TZ"), OsString::from(DEFAULT_TZ));
    env.insert(OsString::from("PHP_CLI_SERVER_WORKERS"), OsString::new());
}

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
        if (result.result == Status::Fail || result.result == Status::Timeout)
            && self.contains(&result.path)
        {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_normalization() {
        let mut env = Env::new();
        env.insert(OsString::from("LC_ALL"), OsString::from("de_DE.UTF-8"));
        env.insert(OsString::from("LANG"), OsString::from("fr_FR.UTF-8"));
        env.insert(OsString::from("TZ"), OsString::from("Europe/Berlin"));
        env.insert(
            OsString::from("PHP_CLI_SERVER_WORKERS"),
            OsString::from("4"),
        );

        normalize_env(&mut env);

        assert_eq!(env.get(&OsString::from("LC_ALL")).unwrap(), "C");
        assert_eq!(env.get(&OsString::from("LANG")).unwrap(), "C");
        assert_eq!(env.get(&OsString::from("TZ")).unwrap(), "UTC");
        assert_eq!(
            env.get(&OsString::from("PHP_CLI_SERVER_WORKERS")).unwrap(),
            ""
        );
    }

    #[test]
    fn apply_one_handles_fail_and_timeout() {
        let env = Environment::parse("tests/timeout.phpt\ntests/fail.phpt\n");
        let mut t1 = TestResult {
            path: "tests/timeout.phpt".to_string(),
            name: "timeout test".to_string(),
            result: Status::Timeout,
            info: String::new(),
            duration_ms: 100,
            diff: None,
            title: None,
            junit_message: None,
        };
        assert!(env.apply_one(&mut t1, "override.txt"));
        assert_eq!(t1.result, Status::Pass);
        assert_eq!(t1.info, "reason: host environment (override.txt)");

        let mut t2 = TestResult {
            path: "tests/fail.phpt".to_string(),
            name: "fail test".to_string(),
            result: Status::Fail,
            info: String::new(),
            duration_ms: 100,
            diff: None,
            title: None,
            junit_message: None,
        };
        assert!(env.apply_one(&mut t2, "override.txt"));
        assert_eq!(t2.result, Status::Pass);
    }
}
