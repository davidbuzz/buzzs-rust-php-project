//! Ports of the ext/fileinfo `.phpt` scenarios that exercise this crate's
//! behaviour (the VM-level ones — argument parsing, cloning, serialization
//! — are left to the binding crate).

use php_fileinfo::{
    Finfo, Warning, FILEINFO_CONTINUE, FILEINFO_DEVICES, FILEINFO_MIME, FILEINFO_MIME_TYPE,
    FILEINFO_NONE, FILEINFO_PRESERVE_ATIME, FILEINFO_RAW, FILEINFO_SYMLINK,
};
use std::path::PathBuf;

fn tests_dir() -> Option<PathBuf> {
    let d =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src/ext/fileinfo/tests");
    d.join("magic").exists().then_some(d)
}

fn b(p: &std::path::Path) -> &[u8] {
    p.as_os_str().as_encoded_bytes()
}

fn quiet() -> impl FnMut(Warning) {
    |w| panic!("unexpected warning {w:?}")
}

fn open(flags: i64, db: Option<&[u8]>) -> Finfo {
    Finfo::open(flags, db, &mut quiet()).expect("finfo_open")
}

fn s(v: Option<Vec<u8>>) -> String {
    String::from_utf8(v.expect("result")).unwrap()
}

#[test]
fn bug57547_directories_and_php() {
    let me = std::env::temp_dir().join(format!("bug57547-{}.php", std::process::id()));
    std::fs::write(&me, "<?php\n$filenames = array(\"..\", __FILE__);\n").unwrap();
    let mut f1 = open(FILEINFO_MIME, None);
    let mut f2 = open(FILEINFO_NONE, None);
    assert_eq!(s(f1.file(b"..", 0, &mut quiet())), "directory");
    assert_eq!(s(f2.file(b"..", FILEINFO_MIME, &mut quiet())), "directory");
    // The .phpt is PHP code, like the original __FILE__.
    assert_eq!(
        s(f1.file(b(&me), 0, &mut quiet())),
        "text/x-php; charset=us-ascii"
    );
    assert_eq!(
        s(f2.file(b(&me), FILEINFO_MIME, &mut quiet())),
        "text/x-php; charset=us-ascii"
    );
    std::fs::remove_file(&me).unwrap();
}

#[test]
fn fixture_files() {
    let Some(dir) = tests_dir() else { return };
    let mut mt = open(FILEINFO_MIME_TYPE, None);
    assert_eq!(
        s(mt.file(b(&dir.join("67647.mov")), 0, &mut quiet())),
        "video/quicktime"
    );
    let mut mime = open(FILEINFO_MIME, None);
    assert_eq!(
        s(mime.file(b(&dir.join("68398.zip")), 0, &mut quiet())),
        "application/zip; charset=binary"
    );
    assert_eq!(
        s(mime.file(b(&dir.join("bug79756.xls")), 0, &mut quiet())),
        "application/vnd.ms-excel; charset=binary"
    );
    let mut none = open(FILEINFO_NONE, None);
    let jpg = s(none.file(b(&dir.join("bug68735.jpg")), 0, &mut quiet()));
    assert!(
        jpg.starts_with(
            "JPEG image data, JFIF standard 1.01, resolution (DPI), density 180x52, segment length 16, comment: \""
        ),
        "{jpg}"
    );
    let gif = std::fs::read(dir.join("68731.gif")).unwrap();
    assert_eq!(s(mt.buffer(&gif, 0, &mut quiet())), "image/gif");
    let gif = dir.join("bug67516.gif");
    assert_eq!(
        s(none.file(b(&gif), FILEINFO_MIME_TYPE, &mut quiet())),
        "image/gif"
    );
    assert_eq!(
        s(none.file(b(&gif), FILEINFO_MIME, &mut quiet())),
        "image/gif; charset=binary"
    );
    // finfo_extension_flag.phpt / finfo_apple_flag.phpt
    let res = dir.join("resources");
    assert_eq!(
        s(none.file(b(&res.join("test.jpg")), 0x0100_0000, &mut quiet())),
        "jpeg/jpg/jpe/jfif"
    );
    for (name, apple) in [
        ("test.jpg", "8BIMJPEG"),
        ("test.gif", "8BIMGIFf"),
        ("test.ppt", "????PPT3"),
    ] {
        assert_eq!(s(none.file(b(&res.join(name)), 0x800, &mut quiet())), apple);
    }
    // bug74170.phpt (locale independence) via mime_content_type().
    assert_eq!(
        s(php_fileinfo::mime_content_type(
            b(&res.join("test.ppt")),
            &mut quiet()
        )),
        "application/vnd.ms-powerpoint"
    );
}

#[test]
fn long_text_lines() {
    // bug68819_002.phpt
    let mut data = b"\r\n''''".to_vec();
    data.extend(std::iter::repeat_n(b'a', 8184));
    data.extend_from_slice(b"say");
    let mut f = open(FILEINFO_NONE, None);
    assert_eq!(
        s(f.buffer(&data, 0, &mut quiet())),
        "ASCII text, with very long lines (8191), with CRLF line terminators"
    );
}

#[test]
fn php_and_python_scripts() {
    // bug69107.phpt
    let mut f = open(FILEINFO_MIME_TYPE, None);
    for src in [
        &b"<?php\nclass A{}"[..],
        b"<?php class A{}",
        b"<?php\tclass A{}",
        b"<?php\n\rclass A{}",
    ] {
        assert_eq!(s(f.buffer(src, 0, &mut quiet())), "text/x-php");
    }
    // bug71434.phpt
    let py =
        b"#!env python\n# -*- coding:utf-8 -*-\nfrom serial import Serial\nfrom sys import exit\n";
    assert_eq!(s(f.buffer(py, 0, &mut quiet())), "text/x-script.python");
    let mut f = open(FILEINFO_NONE, None);
    assert_eq!(
        s(f.buffer(py, 0, &mut quiet())),
        "Python script, ASCII text executable"
    );
}

#[test]
fn bug79283_regex_of_nuls() {
    let db = std::env::temp_dir().join(format!("bug79283-{}.db", std::process::id()));
    std::fs::write(&db, "\n0\tregex\t\\0\\0\\0\\0\tTest\n").unwrap();
    let mut f = open(FILEINFO_NONE, Some(b(&db)));
    assert_eq!(s(f.buffer(b"buffer\n", 0, &mut quiet())), "ASCII text");
    std::fs::remove_file(&db).unwrap();
}

#[test]
fn cdf_unknown_property_id() {
    let Some(dir) = tests_dir() else { return };
    let mut data = std::fs::read(dir.join("resources/test.ppt")).unwrap();
    data[40576] = 0x1f;
    let desc = s(open(FILEINFO_NONE, None).buffer(&data, 0, &mut quiet()));
    assert_eq!(&desc[desc.rfind(", ").unwrap() + 2..], "0x1f: 0");
}

/// `finfo_buffer_basic.phpt` / `finfo_buffer_variation1.phpt`, against the
/// test suite's own text database.
#[test]
fn buffers_with_test_database() {
    let Some(dir) = tests_dir() else { return };
    let magic = dir.join("magic");
    let buffers: [&[u8]; 6] = [
        b"Regular string here",
        b"\x7fELF",
        b"\x00\x00\x001\x00\x00\x000\x00\x00\x000\x00\x00\x002\x00\x00\x000\x00\x00\x000\x00\x00\x003",
        b"\x55\x7A\x6E\x61",
        b"id=ImageMagick\x0a\x0c\x0a:\x1a",
        "RIFFüîò^BAVI LISTv".as_bytes(),
    ];
    let expected = [
        (
            FILEINFO_NONE,
            [
                "ASCII text, with no line terminators",
                "ELF",
                "old ACE/gr binary file",
                "xo65 object,",
                // master's bundled libmagic 5.48 evaluates `&0` after a
                // `search` differently and says "ASCII text"; 5.45 (and
                // PHP 8.3's 5.43) find the MIFF continuation.
                "MIFF image data",
                "RIFF (little-endian) data",
            ],
        ),
        (
            FILEINFO_MIME,
            [
                "text/plain; charset=us-ascii",
                "text/plain; charset=ebcdic",
                "application/octet-stream; charset=binary",
                "text/plain; charset=us-ascii",
                "image/x-miff; charset=binary",
                "text/plain; charset=utf-8",
            ],
        ),
    ];
    for (flags, results) in expected {
        let mut f = open(flags, Some(b(&magic)));
        for (buf, want) in buffers.iter().zip(results) {
            assert_eq!(s(f.buffer(buf, flags, &mut quiet())), want, "flags {flags}");
        }
    }
}

#[test]
fn test_database_open_variations() {
    // finfo_open_basic.phpt / finfo_open_variation1.phpt
    let Some(dir) = tests_dir() else { return };
    let magic = dir.join("magic");
    for flags in [
        FILEINFO_MIME,
        FILEINFO_NONE,
        FILEINFO_SYMLINK,
        FILEINFO_DEVICES,
        FILEINFO_CONTINUE,
        FILEINFO_PRESERVE_ATIME,
        FILEINFO_RAW,
        FILEINFO_MIME | FILEINFO_SYMLINK,
        FILEINFO_DEVICES | FILEINFO_RAW,
    ] {
        open(flags, Some(b(&magic)));
    }
}

#[test]
fn regex_rules_with_test_database() {
    // finfo_file_regex.phpt
    let Some(dir) = tests_dir() else { return };
    let mut f = open(FILEINFO_MIME, Some(b(&dir.join("magic"))));
    let awk = dir.join("resources/test.awk");
    assert_eq!(
        s(f.file(b(&awk), 0, &mut quiet())),
        "text/plain; charset=us-ascii"
    );
    let cont = s(f.file(b(&awk), FILEINFO_CONTINUE, &mut quiet()));
    assert!(
        cont.starts_with("awk") && cont.contains("script, ASCII text"),
        "{cont}"
    );
    // finfo_file_basic.phpt
    let mut f = open(FILEINFO_MIME, None);
    assert_eq!(
        s(f.file(b(&dir.join("magic")), 0, &mut quiet())),
        "text/x-file; charset=us-ascii"
    );
}

#[test]
fn bug68996_non_utf8_database_name() {
    let mut out = Vec::new();
    let r = Finfo::open(
        FILEINFO_MIME_TYPE,
        Some(b"/nonexistent/\xfc\x63"),
        &mut |w| {
            out.push(w);
        },
    );
    assert!(r.is_none());
    assert_eq!(out.len(), 3);
    assert_eq!(
        out[2].message,
        "Failed to load magic database at \"/nonexistent/\u{fffd}c\""
    );
}
