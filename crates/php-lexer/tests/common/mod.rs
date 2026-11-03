//! Shared helpers for integration tests: locating `.phpt` files and
//! extracting their `--FILE--` sections.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// Root of the read-only php-src checkout.
pub fn php_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src")
}

/// All `.phpt` files under the php-src checkout, sorted.
pub fn all_phpt() -> Vec<PathBuf> {
    let mut v = Vec::new();
    collect(&php_src(), &mut v);
    v.sort();
    v
}

/// Every `.phpt` file under php-src paired with its `--FILE--` section.
pub fn corpus() -> Vec<(PathBuf, Vec<u8>)> {
    all_phpt()
        .into_iter()
        .filter_map(|p| {
            let data = std::fs::read(&p).ok()?;
            let src = file_section(&data)?;
            Some((p, src))
        })
        .collect()
}

pub fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "phpt") {
            out.push(p);
        }
    }
}

/// Extracts the `--FILE--` (or `--FILEEOF--`) section of a `.phpt` file.
pub fn file_section(data: &[u8]) -> Option<Vec<u8>> {
    let mut sec: Option<String> = None;
    let mut body = Vec::new();
    let mut found = None;
    for line in data.split_inclusive(|&b| b == b'\n') {
        let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
        let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
        if trimmed.len() > 4
            && trimmed.starts_with(b"--")
            && trimmed.ends_with(b"--")
            && trimmed[2..trimmed.len() - 2]
                .iter()
                .all(|c| c.is_ascii_uppercase() || *c == b'_')
        {
            if matches!(sec.as_deref(), Some("FILE" | "FILEEOF")) && found.is_none() {
                found = Some((sec.clone(), std::mem::take(&mut body)));
            }
            sec = Some(String::from_utf8_lossy(&trimmed[2..trimmed.len() - 2]).into_owned());
            continue;
        }
        if matches!(sec.as_deref(), Some("FILE" | "FILEEOF")) {
            body.extend_from_slice(line);
        }
    }
    if found.is_none() && matches!(sec.as_deref(), Some("FILE" | "FILEEOF")) {
        found = Some((sec, body));
    }
    let (name, mut body) = found?;
    if name.as_deref() == Some("FILEEOF") {
        if body.ends_with(b"\n") {
            body.pop();
        }
        if body.ends_with(b"\r") {
            body.pop();
        }
    }
    Some(body)
}
