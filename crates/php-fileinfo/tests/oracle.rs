// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `finfo_file()` / `finfo_buffer()` over every php-src fixture, compared
//! with results recorded from `/usr/bin/php` (see `data/oracle.tsv`), plus
//! the documented divergences between the system libmagic and PHP's
//! bundled copy (`data/divergent.tsv`).

use php_fileinfo::{Finfo, Warning};
use std::path::PathBuf;

fn ext_root() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src/ext");
    root.join("fileinfo/tests/resources")
        .is_dir()
        .then_some(root)
}

#[allow(clippy::needless_pass_by_value)]
fn no_warnings(w: Warning) {
    panic!("unexpected warning: {w:?}");
}

fn rows(data: &str, cols: usize) -> Vec<Vec<&str>> {
    data.lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let r: Vec<&str> = l.splitn(cols, '\t').collect();
            assert_eq!(r.len(), cols, "bad row {l}");
            r
        })
        .collect()
}

#[test]
fn fixtures_match_php() {
    let Some(root) = ext_root() else { return };
    let mut finfo = Finfo::open(0, None, &mut no_warnings).expect("default database");
    let mut checked = 0;
    for row in rows(include_str!("data/oracle.tsv"), 3) {
        let path = root.join(row[0]);
        let flags: i64 = row[1].parse().unwrap();
        let got = finfo
            .file(path.as_os_str().as_encoded_bytes(), flags, &mut no_warnings)
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&got),
            row[2],
            "{} flags {flags}",
            row[0]
        );
        let data = std::fs::read(&path).unwrap();
        let got = finfo.buffer(&data, flags, &mut no_warnings).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&got),
            row[2],
            "buffer {} flags {flags}",
            row[0]
        );
        checked += 1;
    }
    assert!(checked > 900);
}

#[test]
fn known_divergences_are_stable() {
    let Some(root) = ext_root() else { return };
    let mut finfo = Finfo::open(0, None, &mut no_warnings).expect("default database");
    for row in rows(include_str!("data/divergent.tsv"), 4) {
        let path = root.join(row[0]);
        let flags: i64 = row[1].parse().unwrap();
        let got = finfo
            .file(path.as_os_str().as_encoded_bytes(), flags, &mut no_warnings)
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&got),
            row[2],
            "{} flags {flags}",
            row[0]
        );
        assert_ne!(row[2], row[3]);
    }
}

/// `finfo_upstream.phpt`: libmagic's own regression files, with their
/// optional private `.magic` databases.
#[test]
fn upstream_regression_files() {
    let Some(root) = ext_root() else { return };
    let dir = root.join("fileinfo/tests/upstream");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "testfile").then_some(p)
        })
        .collect();
    names.sort();
    // FIT timestamps are rendered in the local time zone (the .phpt runs
    // with TZ=UTC).
    let known = ["fit-map-data"];
    let mut failures = Vec::new();
    for p in &names {
        let stem = p.file_stem().unwrap().to_str().unwrap();
        let magic = dir.join(format!("{stem}.magic"));
        let expected = std::fs::read_to_string(dir.join(format!("{stem}.result"))).unwrap();
        let db = magic
            .exists()
            .then(|| magic.as_os_str().as_encoded_bytes().to_vec());
        let mut finfo = Finfo::open(0, db.as_deref(), &mut no_warnings).unwrap();
        let got = finfo
            .file(p.as_os_str().as_encoded_bytes(), 0, &mut no_warnings)
            .unwrap();
        if String::from_utf8_lossy(&got) != expected.trim() && !known.contains(&stem) {
            failures.push(stem.to_owned());
        }
    }
    assert!(names.len() > 40);
    assert!(failures.is_empty(), "{failures:?}");
}

/// `finfo_file_002.phpt`.
#[test]
fn resources_mime_types() {
    let Some(root) = ext_root() else { return };
    let expected = [
        ("dir.zip", "application/zip"),
        ("test.awk", "text/plain"),
        ("test.bmp", "image/bmp"),
        ("test.gif", "image/gif"),
        ("test.jpg", "image/jpeg"),
        ("test.json", "application/json"),
        ("test.mp3", "audio/mpeg"),
        ("test.pdf", "application/pdf"),
        ("test.png", "image/png"),
        ("test.ppt", "application/vnd.ms-powerpoint"),
        ("test.tga", "image/x-tga"),
        ("test.webm", "video/webm"),
        ("test.woff", "font/woff"),
        ("test.xz", "application/x-xz"),
    ];
    let mut finfo = Finfo::open(php_fileinfo::FILEINFO_MIME_TYPE, None, &mut no_warnings).unwrap();
    for (name, mime) in expected {
        let p = root.join("fileinfo/tests/resources").join(name);
        let got = finfo
            .file(p.as_os_str().as_encoded_bytes(), 0, &mut no_warnings)
            .unwrap();
        assert_eq!(got, mime.as_bytes(), "{name}");
    }
}
