//! The scope file: path prefixes (relative to the php-src root) of tests that are out of scope
//! for the project. Such tests are not run, are reported with status `OUT`, and are excluded
//! from pass rates and from the baseline.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// File name looked up in the ancestors of the php-src root when `--scope` is not given.
pub const DEFAULT_SCOPE_FILE: &str = "phpt-scope.txt";

/// Out-of-scope path prefixes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    prefixes: Vec<String>,
}

impl Scope {
    /// Parse scope-file text: one prefix per line, `#` starts a comment. A prefix naming a
    /// directory matches everything below it (`ext/ffi` matches `ext/ffi/tests/x.phpt` but not
    /// `ext/ffix/...`).
    pub fn parse(text: &str) -> Self {
        let prefixes = text
            .lines()
            .map(|l| l.split('#').next().unwrap_or_default().trim())
            .filter(|l| !l.is_empty())
            .map(|l| l.trim_start_matches("./").to_string())
            .collect();
        Scope { prefixes }
    }

    /// Load a scope file.
    pub fn load(path: &Path) -> io::Result<Self> {
        Ok(Self::parse(&fs::read_to_string(path)?))
    }

    /// Find [`DEFAULT_SCOPE_FILE`] in `start` or one of its ancestors.
    pub fn find_default(start: &Path) -> Option<PathBuf> {
        start
            .ancestors()
            .map(|a| a.join(DEFAULT_SCOPE_FILE))
            .find(|p| p.is_file())
    }

    /// The configured prefixes.
    pub fn prefixes(&self) -> &[String] {
        &self.prefixes
    }

    /// Is the test at `path` (relative to the php-src root) out of scope?
    pub fn is_out_of_scope(&self, path: &str) -> bool {
        self.prefixes.iter().any(|p| {
            let p = p.trim_end_matches('/');
            path.strip_prefix(p)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_match_whole_components() {
        let s = Scope::parse("# comment\next/ffi\nsapi/fpm/  # trailing\n\n./ext/opcache\n");
        assert_eq!(s.prefixes().len(), 3);
        assert!(s.is_out_of_scope("ext/ffi/tests/001.phpt"));
        assert!(s.is_out_of_scope("sapi/fpm/tests/x.phpt"));
        assert!(s.is_out_of_scope("ext/opcache/tests/jit/a.phpt"));
        assert!(!s.is_out_of_scope("ext/ffix/tests/001.phpt"));
        assert!(!s.is_out_of_scope("Zend/tests/ffi.phpt"));
    }
}
