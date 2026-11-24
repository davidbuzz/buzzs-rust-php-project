// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Test discovery and conflict keys (ref: `run-tests.php` `find_files()`,
//! `run_all_tests_parallel()`).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::phpt::parse_conflicts;

/// Recursively collect `*.phpt` files under `dir` (skipping hidden files and VCS directories),
/// sorted by path.
pub fn find_tests(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir {
            if name != ".svn" && name != ".git" {
                walk(&path, out);
            }
        } else if name.ends_with(".phpt") && !name.starts_with('.') {
            out.push(path);
        }
    }
}

/// Resolve command-line paths (files or directories) into a sorted, de-duplicated list of
/// canonical test paths.
pub fn collect(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for p in paths {
        let canon = p
            .canonicalize()
            .map_err(|_| format!("Cannot find test file \"{}\".", p.display()))?;
        if canon.is_dir() {
            out.extend(find_tests(&canon));
        } else if canon.extension().is_some_and(|e| e == "phpt") {
            out.push(canon);
        } else {
            return Err(format!("Cannot find test file \"{}\".", p.display()));
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Guess the php-src root: the nearest ancestor of `path` containing `run-tests.php`.
pub fn find_srcdir(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|a| a.join("run-tests.php").is_file())
        .map(Path::to_path_buf)
}

/// Extract the `--CONFLICTS--` section text (`/^--CONFLICTS--(.+?)^--/ms`).
fn conflicts_section(content: &[u8]) -> Option<&[u8]> {
    let marker = b"--CONFLICTS--";
    let mut pos = 0;
    let start = loop {
        let line_end = content[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(content.len(), |p| pos + p + 1);
        if content[pos..].starts_with(marker) {
            break pos + marker.len();
        }
        if line_end >= content.len() {
            return None;
        }
        pos = line_end;
    };
    let mut line_start = content[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|p| start + p + 1)?;
    while line_start < content.len() {
        if content[line_start..].starts_with(b"--") {
            return Some(&content[start..line_start]);
        }
        line_start = content[line_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(content.len(), |p| line_start + p + 1);
    }
    None
}

/// Computes conflict keys per test, caching directory `CONFLICTS` files.
#[derive(Debug, Default)]
pub struct ConflictResolver {
    dirs: HashMap<PathBuf, Vec<String>>,
}

impl ConflictResolver {
    /// Conflict keys for `test`: its `--CONFLICTS--` section, else its directory's `CONFLICTS`
    /// file. Empty strings are dropped.
    pub fn conflicts_for(&mut self, test: &Path) -> Vec<String> {
        let content = fs::read(test).unwrap_or_default();
        let keys = if let Some(section) = conflicts_section(&content) {
            parse_conflicts(section)
        } else {
            let dir = test.parent().unwrap_or(Path::new(".")).to_path_buf();
            self.dirs
                .entry(dir)
                .or_insert_with_key(|dir| {
                    fs::read(dir.join("CONFLICTS"))
                        .map(|c| parse_conflicts(&c))
                        .unwrap_or_default()
                })
                .clone()
        };
        keys.into_iter().filter(|k| !k.is_empty()).collect()
    }
}

/// File name looked up in the ancestors of the php-src root when `--extra-conflicts` is not
/// given.
pub const DEFAULT_EXTRA_CONFLICTS_FILE: &str = "phpt-conflicts.txt";

/// Conflict keys that php-src's tests lack: tests that share a file, port or other resource
/// without a `--CONFLICTS--` section or directory `CONFLICTS` file saying so. Like the keys
/// php-src declares, tests sharing a key never run at the same time. php-src is read-only
/// here, so the repo records them in its own file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtraConflicts {
    keys: HashMap<String, Vec<String>>,
}

impl ExtraConflicts {
    /// Parse the file's text: one test path (relative to the php-src root) per line, followed
    /// by its conflict keys, separated by white space; `#` starts a comment.
    pub fn parse(text: &str) -> Self {
        let mut keys: HashMap<String, Vec<String>> = HashMap::new();
        for line in text.lines() {
            let mut words = line
                .split('#')
                .next()
                .unwrap_or_default()
                .split_whitespace();
            let Some(path) = words.next() else {
                continue;
            };
            keys.entry(path.trim_start_matches("./").to_string())
                .or_default()
                .extend(words.map(ToString::to_string));
        }
        ExtraConflicts { keys }
    }

    /// Load the file.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        Ok(Self::parse(&fs::read_to_string(path)?))
    }

    /// Find [`DEFAULT_EXTRA_CONFLICTS_FILE`] in `start` or one of its ancestors.
    pub fn find_default(start: &Path) -> Option<PathBuf> {
        start
            .ancestors()
            .map(|a| a.join(DEFAULT_EXTRA_CONFLICTS_FILE))
            .find(|p| p.is_file())
    }

    /// The extra keys of the test at `path` (relative to the php-src root).
    pub fn keys_for(&self, path: &str) -> &[String] {
        self.keys
            .get(path.trim_start_matches("./"))
            .map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflicts_section_extraction() {
        let t = b"--TEST--\nx\n--CONFLICTS--\nserver\n# c\n--FILE--\n<?php\n";
        assert_eq!(conflicts_section(t), Some(&b"\nserver\n# c\n"[..]));
        assert_eq!(conflicts_section(b"--TEST--\nx\n--FILE--\n"), None);
    }

    #[test]
    fn extra_conflicts_file() {
        let x = ExtraConflicts::parse(
            "# shared files\n\
             ext/a/tests/one.phpt  a.pem.tmp   # writes a.pem.tmp\n\
             ./ext/a/tests/two.phpt a.pem.tmp port\n\
             \n\
             ext/a/tests/two.phpt more\n",
        );
        assert_eq!(x.keys_for("ext/a/tests/one.phpt"), ["a.pem.tmp"]);
        assert_eq!(
            x.keys_for("./ext/a/tests/two.phpt"),
            ["a.pem.tmp", "port", "more"]
        );
        assert!(x.keys_for("ext/a/tests/three.phpt").is_empty());
    }
}
