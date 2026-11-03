//! Oracle tests for `highlight_string()` and `php_strip_whitespace()`:
//! hand-written snippets (byte-exact) and a corpus of php-src `.phpt`
//! FILE sections (md5 of the outputs; skipped when `modules/php-src` is
//! absent).

mod common;

use common::{fields, fixture_lines, show, unhex};
use php_stdmisc::highlight::{highlight, strip_whitespace, HighlightColors};

#[test]
fn snippets_match_oracle() {
    let colors = HighlightColors::default();
    let custom = HighlightColors {
        keyword: b"#0000BB".to_vec(),
        string: b"red".to_vec(),
        html: b"#0000BB".to_vec(),
        ..HighlightColors::default()
    };
    let mut n = 0;
    for line in fixture_lines("highlight.txt") {
        let f = fields(&line);
        match f[0] {
            "snip" => {
                let src = unhex(f[1]);
                assert_eq!(
                    show(&highlight(&src, &colors, true)),
                    show(&unhex(f[2])),
                    "highlight {:?}",
                    show(&src)
                );
                assert_eq!(
                    show(&strip_whitespace(&src, true).0),
                    show(&unhex(f[3])),
                    "strip {:?}",
                    show(&src)
                );
                n += 1;
            }
            "colors" => {
                let src = unhex(f[1]);
                assert_eq!(
                    show(&highlight(&src, &custom, true)),
                    show(&unhex(f[2])),
                    "colors {:?}",
                    show(&src)
                );
            }
            _ => {}
        }
    }
    assert!(n > 40);
}

/// Extracts the FILE section like the generator's regex
/// `/^--FILE--\r?\n(.*?)\r?\n--[A-Z_]+--\r?$/ms`.
fn file_section(src: &[u8]) -> Option<Vec<u8>> {
    let lines: Vec<&[u8]> = src.split(|&c| c == b'\n').collect();
    let strip_cr = |l: &[u8]| l.strip_suffix(b"\r").map_or(l.to_vec(), <[u8]>::to_vec);
    let start = lines.iter().position(|l| strip_cr(l) == b"--FILE--")?;
    let is_section = |l: &[u8]| {
        let l = strip_cr(l);
        l.len() > 4
            && l.starts_with(b"--")
            && l.ends_with(b"--")
            && l[2..l.len() - 2]
                .iter()
                .all(|&c| c.is_ascii_uppercase() || c == b'_')
    };
    let end = (start + 2..lines.len()).find(|&i| is_section(lines[i]))?;
    let mut body = lines[start + 1..end].join(&b'\n');
    if body.ends_with(b"\r") {
        body.pop();
    }
    Some(body)
}

#[test]
fn phpt_corpus_matches_oracle() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src");
    if !root.exists() {
        eprintln!("modules/php-src not present; skipping corpus");
        return;
    }
    let colors = HighlightColors::default();
    let (mut n, mut bad) = (0, Vec::new());
    for line in fixture_lines("highlight.txt") {
        let f = fields(&line);
        if f[0] != "corpus" {
            continue;
        }
        let Ok(src) = std::fs::read(root.join(f[1])) else {
            continue;
        };
        let Some(code) = file_section(&src) else {
            continue;
        };
        let h = php_hash::bin2hex(&php_hash::md5(&highlight(&code, &colors, true)));
        let s = php_hash::bin2hex(&php_hash::md5(&strip_whitespace(&code, true).0));
        // php-src master tokens (`private(set)`, `(void)` casts, partial
        // application) are scanned differently by the 8.3 oracle.
        let master_only = [
            "asymmetric_visibility/",
            "partial_application/",
            "property_hooks/gh15644",
            "property_hooks/gh19044",
            "cast_to_void",
        ];
        if master_only.iter().any(|m| f[1].contains(m)) && s == f[3] {
            continue;
        }
        if h != f[2] || s != f[3] {
            bad.push(format!(
                "{} (highlight {}, strip {})",
                f[1],
                h == f[2],
                s == f[3]
            ));
        }
        n += 1;
    }
    assert!(n > 1000, "{n}");
    assert!(
        bad.is_empty(),
        "{} of {n} differ: {:#?}",
        bad.len(),
        &bad[..bad.len().min(30)]
    );
}
