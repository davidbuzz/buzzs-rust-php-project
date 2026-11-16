// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `opcache.blacklist_filename`: the files opcache never caches.
//!
//! ref: ext/opcache/zend_accelerator_blacklist.c

use std::path::{Component, Path, PathBuf};

/// The loaded blacklist (`zend_blacklist`): its entries in load order and
/// the warnings `zend_accel_error(ACCEL_LOG_WARNING, ...)` reports while
/// loading it (each ending in its own newline, as php-src's do).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Blacklist {
    /// The expanded entry paths (`opcache_get_configuration()['blacklist']`).
    pub entries: Vec<String>,
    /// Load warnings.
    pub warnings: Vec<String>,
}

impl Blacklist {
    /// `zend_accel_blacklist_load()`: loads every file matching the glob
    /// `pattern`, in sorted order.
    #[must_use]
    pub fn load(pattern: &str) -> Self {
        let mut list = Self::default();
        let files = glob(pattern);
        if files.is_empty() {
            list.warnings
                .push(format!("No blacklist file found matching: {pattern}\n"));
            return list;
        }
        for file in files {
            match std::fs::read(&file) {
                Ok(contents) => {
                    let dir = std::fs::canonicalize(&file)
                        .ok()
                        .and_then(|p| p.parent().map(Path::to_path_buf));
                    list.entries.extend(parse(&contents, dir.as_deref()));
                }
                Err(_) => list.warnings.push(format!(
                    "Cannot load blacklist file: {}\n",
                    file.to_string_lossy()
                )),
            }
        }
        list
    }

    /// `zend_accel_blacklist_is_blacklisted()`: whether some entry matches
    /// a prefix of `path` (`?` is one character other than `/`, `*` any
    /// run of them, `**` any run of characters).
    #[must_use]
    pub fn is_blacklisted(&self, path: &[u8]) -> bool {
        self.entries
            .iter()
            .any(|e| prefix_matches(e.as_bytes(), path))
    }
}

/// `zend_accel_blacklist_loadone()`: one entry per line, with a trailing
/// CR/LF and leading CRs removed, surrounding double quotes stripped,
/// blank lines and `;` comments skipped, relative entries expanded against
/// the blacklist file's directory (`expand_filepath_ex`).
fn parse(contents: &[u8], dir: Option<&Path>) -> Vec<String> {
    let mut entries = Vec::new();
    for line in contents.split(|&b| b == b'\n') {
        let mut line = line.strip_suffix(b"\r").unwrap_or(line);
        while let Some(rest) = line.strip_prefix(b"\r") {
            line = rest;
        }
        if line.len() >= 2 && line.first() == Some(&b'"') && line.last() == Some(&b'"') {
            line = &line[1..line.len() - 1];
        }
        if line.is_empty() || line.first() == Some(&b';') {
            continue;
        }
        let entry = String::from_utf8_lossy(line);
        entries.push(expand(&entry, dir));
    }
    entries
}

/// `expand_filepath_ex()`: an absolute path, or a relative one joined to
/// `dir` (the working directory when unknown), with `.` and `..`
/// components resolved lexically.
fn expand(entry: &str, dir: Option<&Path>) -> String {
    let path = Path::new(entry);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let base = dir
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        base.join(path)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().into_owned()
}

/// The files matching a glob whose wildcards are in its last component,
/// sorted (`php_glob()` without flags sorts its results). A pattern
/// without wildcards matches the file it names, if it exists.
fn glob(pattern: &str) -> Vec<PathBuf> {
    let path = Path::new(pattern);
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return Vec::new();
    };
    if !name.contains(|c| matches!(c, '*' | '?' | '[')) {
        return if path.exists() {
            vec![path.to_path_buf()]
        } else {
            Vec::new()
        };
    }
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .filter(|e| {
            let file = e.file_name();
            let file = file.to_string_lossy();
            // A leading `.` is matched only by a literal one (`fnmatch`
            // with `FNM_PERIOD`).
            (!file.starts_with('.') || name.starts_with('.'))
                && fnmatch(name.as_bytes(), file.as_bytes())
        })
        .map(|e| dir.join(e.file_name()))
        .collect();
    found.sort();
    found
}

/// `fnmatch()` of one path component: `*`, `?` and `[...]` (with `!`/`^`
/// negation and ranges).
fn fnmatch(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|i| fnmatch(rest, &name[i..])),
        Some((b'?', rest)) => !name.is_empty() && fnmatch(rest, &name[1..]),
        Some((b'[', rest)) => {
            let Some((&c, tail)) = name.split_first() else {
                return false;
            };
            match bracket(rest, c) {
                Some((true, after)) => fnmatch(after, tail),
                Some((false, _)) => false,
                // An unterminated `[` is literal.
                None => c == b'[' && fnmatch(rest, tail),
            }
        }
        Some((&p, rest)) => name.first() == Some(&p) && fnmatch(rest, &name[1..]),
    }
}

/// A `[...]` set after its `[`: whether `c` is in it, and the pattern
/// after the closing `]`; `None` when it is not closed.
fn bracket(set: &[u8], c: u8) -> Option<(bool, &[u8])> {
    let (negate, mut i) = match set.first() {
        Some(b'!' | b'^') => (true, 1),
        _ => (false, 0),
    };
    let mut matched = false;
    let mut first = true;
    loop {
        let &b = set.get(i)?;
        if b == b']' && !first {
            return Some((matched != negate, &set[i + 1..]));
        }
        first = false;
        if set.get(i + 1) == Some(&b'-') && set.get(i + 2).is_some_and(|&e| e != b']') {
            let hi = set[i + 2];
            matched |= (b..=hi).contains(&c);
            i += 3;
        } else {
            matched |= b == c;
            i += 1;
        }
    }
}

/// Whether the blacklist entry `pattern` matches a prefix of `path`, as
/// the `^(...)` regular expression `zend_accel_blacklist_update_regexp()`
/// builds from it does.
fn prefix_matches(pattern: &[u8], path: &[u8]) -> bool {
    match pattern.split_first() {
        None => true,
        Some((b'*', rest)) => {
            if let Some(rest) = rest.strip_prefix(b"*") {
                (0..=path.len()).any(|i| prefix_matches(rest, &path[i..]))
            } else {
                let run = path.iter().take_while(|&&b| b != b'/').count();
                (0..=run).any(|i| prefix_matches(rest, &path[i..]))
            }
        }
        Some((b'?', rest)) => {
            path.first().is_some_and(|&b| b != b'/') && prefix_matches(rest, &path[1..])
        }
        Some((&p, rest)) => path.first() == Some(&p) && prefix_matches(rest, &path[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_entries_like_php() {
        let contents =
            b"; comment\n\n/path/to/foo\n\"/path/to/foo2\"\r\n/path/to/bar\nrel.inc\n./cur.php\n";
        let entries = parse(contents, Some(Path::new("/base/dir")));
        assert_eq!(
            entries,
            vec![
                "/path/to/foo",
                "/path/to/foo2",
                "/path/to/bar",
                "/base/dir/rel.inc",
                "/base/dir/cur.php",
            ]
        );
    }

    #[test]
    fn matches_prefixes_with_wildcards() {
        let list = Blacklist {
            entries: vec![
                "/tmp/path/?nocache.inc".into(),
                "/tmp/path/*/somedir".into(),
                "/a/**/z".into(),
                "/exact/file.php".into(),
            ],
            warnings: Vec::new(),
        };
        assert!(list.is_blacklisted(b"/tmp/path/Xnocache.inc"));
        assert!(!list.is_blacklisted(b"/tmp/path//nocache.inc"));
        assert!(list.is_blacklisted(b"/tmp/path/one/somedir/file.php"));
        assert!(!list.is_blacklisted(b"/tmp/path/one/two/somedir"));
        assert!(list.is_blacklisted(b"/a/b/c/z"));
        assert!(list.is_blacklisted(b"/exact/file.php"));
        assert!(list.is_blacklisted(b"/exact/file.phpx"));
        assert!(!list.is_blacklisted(b"/exact/file.ph"));
    }

    #[test]
    fn fnmatch_sets_and_wildcards() {
        assert!(fnmatch(b"opcache-*.blacklist", b"opcache-1.blacklist"));
        assert!(!fnmatch(b"opcache-*.blacklist", b"opcache-1.blacklistx"));
        assert!(fnmatch(b"a?c", b"abc"));
        assert!(fnmatch(b"[a-c]x", b"bx"));
        assert!(!fnmatch(b"[!a-c]x", b"bx"));
        assert!(fnmatch(b"[x", b"[x"));
    }

    #[test]
    fn missing_file_warns() {
        let list = Blacklist::load("/nonexistent-dir-for-phprust-test/*.blacklist");
        assert!(list.entries.is_empty());
        assert_eq!(
            list.warnings,
            vec![
                "No blacklist file found matching: /nonexistent-dir-for-phprust-test/*.blacklist\n"
            ]
        );
    }
}
