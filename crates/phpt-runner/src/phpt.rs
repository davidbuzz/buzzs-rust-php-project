//! Parser for the `.phpt` file format (ref: `run-tests.php`, class `TestFile`).

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::bytes::{php_empty, replace, trim};

/// Every section name `run-tests.php` accepts; anything else borks the test.
pub const ALLOWED_SECTIONS: &[&str] = &[
    "EXPECT",
    "EXPECTF",
    "EXPECTREGEX",
    "EXPECTREGEX_EXTERNAL",
    "EXPECT_EXTERNAL",
    "EXPECTF_EXTERNAL",
    "EXPECTHEADERS",
    "POST",
    "POST_RAW",
    "GZIP_POST",
    "DEFLATE_POST",
    "PUT",
    "GET",
    "COOKIE",
    "ARGS",
    "FILE",
    "FILEEOF",
    "FILE_EXTERNAL",
    "REDIRECTTEST",
    "CAPTURE_STDIO",
    "STDIN",
    "CGI",
    "PHPDBG",
    "INI",
    "ENV",
    "EXTENSIONS",
    "SKIPIF",
    "XFAIL",
    "XLEAK",
    "CLEAN",
    "CREDITS",
    "DESCRIPTION",
    "CONFLICTS",
    "WHITESPACE_SENSITIVE",
    "FLAKY",
];

/// A test file that cannot be run because it is malformed ("BORK" in run-tests terms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorkError(pub String);

impl fmt::Display for BorkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BorkError {}

/// Which kind of expectation a test uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectKind {
    /// `--EXPECT--`: exact comparison.
    Exact,
    /// `--EXPECTF--`: format placeholders.
    Format,
    /// `--EXPECTREGEX--`: a raw PCRE pattern.
    Regex,
}

/// A parsed `.phpt` test file.
#[derive(Debug, Clone)]
pub struct TestFile {
    path: PathBuf,
    sections: HashMap<String, Vec<u8>>,
}

impl TestFile {
    /// Read and parse a test file from disk.
    pub fn load(path: &Path, in_redirect: bool) -> Result<Self, BorkError> {
        let content = fs::read(path)
            .map_err(|e| BorkError(format!("cannot read test [{}]: {e}", path.display())))?;
        Self::parse(path, &content, in_redirect)
    }

    /// Parse test-file `content`; `path` is used to resolve `*_EXTERNAL` sections and in messages.
    pub fn parse(path: &Path, content: &[u8], in_redirect: bool) -> Result<Self, BorkError> {
        let mut test = TestFile {
            path: path.to_path_buf(),
            sections: read_sections(path, content)?,
        };
        test.validate_and_process(in_redirect)?;
        Ok(test)
    }

    /// The path the test was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `isset($sections[$name])`.
    pub fn has_section(&self, name: &str) -> bool {
        self.sections.contains_key(name)
    }

    /// True if any of the named sections exists.
    pub fn has_any_section(&self, names: &[&str]) -> bool {
        names.iter().any(|n| self.has_section(n))
    }

    /// `!empty($sections[$name])` (PHP semantics: `"0"` counts as empty).
    pub fn section_not_empty(&self, name: &str) -> bool {
        self.sections.get(name).is_some_and(|s| !php_empty(s))
    }

    /// The raw section text, if present.
    pub fn section(&self, name: &str) -> Option<&[u8]> {
        self.sections.get(name).map(Vec::as_slice)
    }

    /// The raw section text, or an empty slice.
    pub fn section_or_empty(&self, name: &str) -> &[u8] {
        self.section(name).unwrap_or_default()
    }

    /// Override a section (used when SKIPIF prints `xfail`, `flaky`, ...).
    pub fn set_section(&mut self, name: &str, value: Vec<u8>) {
        self.sections.insert(name.to_string(), value);
    }

    /// Names of all sections present, sorted.
    pub fn section_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.sections.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// `trim(--TEST--)`.
    pub fn name(&self) -> String {
        String::from_utf8_lossy(trim(self.section_or_empty("TEST"))).into_owned()
    }

    /// Tests that need the CGI SAPI (`TestFile::isCGI`).
    pub fn is_cgi(&self) -> bool {
        self.has_section("CGI")
            || [
                "GET",
                "POST",
                "GZIP_POST",
                "DEFLATE_POST",
                "POST_RAW",
                "PUT",
                "COOKIE",
                "EXPECTHEADERS",
            ]
            .iter()
            .any(|s| self.section_not_empty(s))
    }

    /// The expectation kind and its raw text. Validation guarantees exactly one exists unless the
    /// test is a REDIRECTTEST.
    pub fn expectation(&self) -> Option<(ExpectKind, &[u8])> {
        if let Some(s) = self.section("EXPECTF") {
            Some((ExpectKind::Format, s))
        } else if let Some(s) = self.section("EXPECTREGEX") {
            Some((ExpectKind::Regex, s))
        } else {
            self.section("EXPECT").map(|s| (ExpectKind::Exact, s))
        }
    }

    /// Conflict keys from the `--CONFLICTS--` section, if any.
    pub fn conflicts(&self) -> Option<Vec<String>> {
        self.section("CONFLICTS").map(parse_conflicts)
    }

    fn validate_and_process(&mut self, in_redirect: bool) -> Result<(), BorkError> {
        if self.has_section("REDIRECTTEST") {
            if in_redirect {
                return Err(BorkError(
                    "Can't redirect a test from within a redirected test".into(),
                ));
            }
            return Ok(());
        }
        let file_sections = ["FILE", "FILEEOF", "FILE_EXTERNAL"]
            .iter()
            .filter(|s| self.has_section(s))
            .count();
        if !self.has_section("PHPDBG") && file_sections != 1 {
            return Err(BorkError("missing section --FILE--".into()));
        }

        if let Some(eof) = self.sections.remove("FILEEOF") {
            let end = eof
                .iter()
                .rposition(|&b| b != b'\r' && b != b'\n')
                .map_or(0, |p| p + 1);
            self.sections.insert("FILE".into(), eof[..end].to_vec());
        }

        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        for prefix in ["FILE", "EXPECT", "EXPECTF", "EXPECTREGEX"] {
            let key = format!("{prefix}_EXTERNAL");
            if let Some(name) = self.sections.get(&key) {
                let name = replace(name, b"..", b"");
                let name = String::from_utf8_lossy(trim(&name)).into_owned();
                let file = dir.join(&name);
                let data = fs::read(&file).map_err(|_| {
                    BorkError(format!(
                        "could not load --{key}-- {}/{}",
                        dir.display(),
                        file.display()
                    ))
                })?;
                self.sections.insert(prefix.to_string(), data);
            }
        }

        let expects = ["EXPECT", "EXPECTF", "EXPECTREGEX"]
            .iter()
            .filter(|s| self.has_section(s))
            .count();
        if expects != 1 {
            return Err(BorkError(
                "missing section --EXPECT--, --EXPECTF-- or --EXPECTREGEX--".into(),
            ));
        }

        if self.has_section("PHPDBG") && !self.has_section("STDIN") {
            let mut stdin = self.section_or_empty("PHPDBG").to_vec();
            stdin.push(b'\n');
            self.sections.insert("STDIN".into(), stdin);
        }
        Ok(())
    }
}

/// Parse a section header line (`/^--([_A-Z]+)--/`), returning the section name.
fn section_header(line: &[u8]) -> Option<&str> {
    let rest = line.strip_prefix(b"--")?;
    let len = rest
        .iter()
        .take_while(|b| b.is_ascii_uppercase() || **b == b'_')
        .count();
    if len == 0 || !rest[len..].starts_with(b"--") {
        return None;
    }
    std::str::from_utf8(&rest[..len]).ok()
}

/// `preg_match('/^===DONE===\s*$/', $line)`.
fn is_done_marker(line: &[u8]) -> bool {
    line.strip_prefix(b"===DONE===")
        .is_some_and(|rest| rest.iter().all(u8::is_ascii_whitespace))
}

fn read_sections(path: &Path, content: &[u8]) -> Result<HashMap<String, Vec<u8>>, BorkError> {
    if content.is_empty() {
        return Err(BorkError(format!("empty test [{}]", path.display())));
    }
    let mut lines = content.split_inclusive(|&b| b == b'\n');
    let first = lines.next().unwrap_or_default();
    if !first.starts_with(b"--TEST--") {
        return Err(BorkError(format!(
            "tests must start with --TEST-- [{}]",
            path.display()
        )));
    }

    let mut sections: HashMap<String, Vec<u8>> = HashMap::new();
    sections.insert("TEST".into(), Vec::new());
    let mut section = String::from("TEST");
    let mut secfile = false;
    let mut secdone = false;

    for line in lines {
        if let Some(name) = section_header(line) {
            if sections.get(name).is_some_and(|s| !php_empty(s)) {
                return Err(BorkError(format!("duplicated {name} section")));
            }
            if !ALLOWED_SECTIONS.contains(&name) {
                return Err(BorkError(format!("Unknown section \"{name}\"")));
            }
            name.clone_into(&mut section);
            sections.insert(section.clone(), Vec::new());
            secfile = matches!(name, "FILE" | "FILEEOF" | "FILE_EXTERNAL");
            secdone = false;
            continue;
        }
        if !secdone {
            sections
                .entry(section.clone())
                .or_default()
                .extend_from_slice(line);
        }
        if secfile && is_done_marker(line) {
            secdone = true;
        }
    }
    Ok(sections)
}

/// `parse_conflicts()`: strip `#` comments, split lines, trim each.
pub fn parse_conflicts(text: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(text);
    let stripped: String = text
        .lines()
        .map(|l| l.find('#').map_or(l, |p| &l[..p]))
        .collect::<Vec<_>>()
        .join("\n");
    stripped
        .trim()
        .lines()
        .map(|s| s.trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<TestFile, BorkError> {
        TestFile::parse(Path::new("/nonexistent/t.phpt"), s.as_bytes(), false)
    }

    #[test]
    fn basic_sections() {
        let t = parse("--TEST--\nhello\n--FILE--\n<?php echo 1;\n--EXPECT--\n1\n").unwrap();
        assert_eq!(t.name(), "hello");
        assert_eq!(t.section("FILE").unwrap(), b"<?php echo 1;\n");
        assert_eq!(t.expectation().unwrap(), (ExpectKind::Exact, &b"1\n"[..]));
    }

    #[test]
    fn errors() {
        assert!(parse("").is_err());
        assert!(parse("--TEST\n").is_err());
        assert_eq!(
            parse("--TEST--\nx\n--FOO--\n").unwrap_err().0,
            "Unknown section \"FOO\""
        );
        assert_eq!(
            parse("--TEST--\nx\n--EXPECT--\n1\n").unwrap_err().0,
            "missing section --FILE--"
        );
        assert!(parse("--TEST--\nx\n--FILE--\n--EXPECT--\na\n--EXPECTF--\nb\n").is_err());
        assert_eq!(
            parse("--TEST--\nx\n--FILE--\na\n--FILE--\nb\n--EXPECT--\n")
                .unwrap_err()
                .0,
            "duplicated FILE section"
        );
        // An empty duplicate is tolerated, like in run-tests.php.
        assert!(parse("--TEST--\nx\n--FILE--\n--FILE--\nb\n--EXPECT--\n").is_ok());
    }

    #[test]
    fn done_marker_and_fileeof() {
        let t = parse("--TEST--\nx\n--FILE--\na\n===DONE===\nignored\n--EXPECT--\n").unwrap();
        assert_eq!(t.section("FILE").unwrap(), b"a\n===DONE===\n");
        let t = parse("--TEST--\nx\n--FILEEOF--\n<?php\r\n\n--EXPECT--\n").unwrap();
        assert_eq!(t.section("FILE").unwrap(), b"<?php");
        assert!(!t.has_section("FILEEOF"));
    }

    #[test]
    fn header_rest_of_line_ignored() {
        assert_eq!(section_header(b"--EXPECT-- trailing\n"), Some("EXPECT"));
        assert_eq!(section_header(b"--expect--\n"), None);
        assert_eq!(section_header(b"----\n"), None);
    }

    #[test]
    fn cgi_and_conflicts() {
        let t = parse("--TEST--\nx\n--GET--\na=b\n--FILE--\n--EXPECT--\n").unwrap();
        assert!(t.is_cgi());
        let t = parse("--TEST--\nx\n--GET--\n--FILE--\n--EXPECT--\n").unwrap();
        assert!(!t.is_cgi());
        assert_eq!(
            parse_conflicts(b"# comment\nall\n  foo # x\n"),
            vec!["all", "foo"]
        );
    }
}
