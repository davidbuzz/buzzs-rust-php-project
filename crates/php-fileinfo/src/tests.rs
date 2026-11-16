// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
use super::*;
use std::io::Cursor;
use std::path::PathBuf;

/// Runs `f` with a warning sink and returns the warnings it produced.
fn warnings<T>(f: impl FnOnce(&mut dyn FnMut(Warning)) -> T) -> (T, Vec<Warning>) {
    let mut out = Vec::new();
    let r = f(&mut |w| out.push(w));
    (r, out)
}

fn messages(ws: &[Warning]) -> Vec<String> {
    ws.iter()
        .map(|w| match &w.param {
            Some(p) => format!("({p}) {}", w.message),
            None => w.message.clone(),
        })
        .collect()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("php-fileinfo-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn bytes(p: &std::path::Path) -> &[u8] {
    p.as_os_str().as_bytes()
}

fn default_finfo(flags: i64) -> Finfo {
    let (f, w) = warnings(|warn| Finfo::open(flags, None, warn));
    assert!(w.is_empty());
    f.unwrap()
}

#[test]
fn constants() {
    assert_eq!(FILEINFO_MIME, 1040);
    assert_eq!(FILEINFO_EXTENSION, 16_777_216);
    assert_eq!(CONSTANTS.len(), 11);
    assert_eq!(CONSTANTS[3], ("FILEINFO_MIME_TYPE", 16));
}

#[test]
fn buffer_basics() {
    let mut f = default_finfo(FILEINFO_NONE);
    let (r, w) = warnings(|warn| f.buffer(b"Regular string here", 0, warn));
    assert!(w.is_empty());
    assert_eq!(r.unwrap(), b"ASCII text, with no line terminators");
    let (r, _) = warnings(|warn| f.buffer(b"", 0, warn));
    assert_eq!(r.unwrap(), b"empty");
    // gh13344.phpt
    let (r, _) =
        warnings(|warn| f.buffer(b"\xff\xfe\x00\x00\x00\x09\x99\x99", FILEINFO_MIME, warn));
    assert_eq!(r.unwrap(), b"application/octet-stream; charset=utf-32le");
    // Per-call flags are restored afterwards.
    let (r, _) =
        warnings(|warn| f.buffer(b"GIF89a\x01\x00\x01\x00\x00\x00", FILEINFO_MIME_TYPE, warn));
    assert_eq!(r.unwrap(), b"image/gif");
    let (r, _) = warnings(|warn| f.buffer(b"<?php echo 1;\n", 0, warn));
    assert_eq!(r.unwrap(), b"PHP script, ASCII text");
}

/// The default database carries php-src 5.48's entries (`overlay.magic`).
#[test]
fn default_database_overlay() {
    let mut f = default_finfo(FILEINFO_MIME_TYPE);
    // gh17039.phpt: a closure no longer makes PHP look like JavaScript.
    let php = b"<?php\n\nclass Foo\n{\n    public function bar()\n    {\n        $this->baz(function () {});\n    }\n}\n";
    let (r, w) = warnings(|warn| f.buffer(php, 0, warn));
    assert_eq!(r.unwrap(), b"text/x-php");
    assert!(w.is_empty());
    // `FILEINFO_NONE` (0) sets no per-call options (`_php_finfo_get_type`:
    // `if (options)`), so the finfo's own MIME_TYPE flags stay in force.
    let (r, _) = warnings(|warn| f.buffer(php, FILEINFO_NONE, warn));
    assert_eq!(r.unwrap(), b"text/x-php");
    // FILEINFO_CONTINUE runs on the plain database, so the overlaid `<?php`
    // entry is not reported twice.
    let (r, _) = warnings(|warn| f.buffer(b"<?php echo 1;\n", FILEINFO_CONTINUE, warn));
    assert_eq!(r.unwrap(), b"PHP script, ASCII text");
    // A named-block override (`arj-archive`, upstream/arj.testfile) reached
    // through the system database's own top-level test.
    let mut arj = vec![
        0x60, 0xea, 0x32, 0x00, 0x22, 0x0b, 0x01, 0x02, 0x10, 0x00, 0x02, 0x4e, 0xe4, 0x3b, 0xe5,
        0x61, 0xe4, 0x3b, 0xe5, 0x61,
    ];
    arj.resize(38, 0);
    arj.extend_from_slice(b"example_m0.arj\0\0");
    let mut f = default_finfo(FILEINFO_NONE);
    let (r, _) = warnings(|warn| f.buffer(&arj, 0, warn));
    assert_eq!(
        String::from_utf8(r.unwrap()).unwrap(),
        "ARJ archive data, v11, slash-switched, created Jan 05 2028 07:31:08, \
         modified Jan 05 2028 07:31:08, original name: example_m0.arj, os: Unix"
    );
}

/// libmagic 5.48 counts SUB (`^Z`) as a text character.
#[test]
fn sub_is_text() {
    let mut f = default_finfo(FILEINFO_NONE);
    let (r, _) = warnings(|warn| f.buffer(b"a\x1ab\n", 0, warn));
    assert_eq!(r.unwrap(), b"ASCII text");
    let (r, _) = warnings(|warn| f.buffer(b"a\x1ab\n", FILEINFO_MIME, warn));
    assert_eq!(r.unwrap(), b"text/plain; charset=us-ascii");
    let p = scratch("sub.txt");
    std::fs::write(&p, b"a\x1ab\n").unwrap();
    let (r, _) = warnings(|warn| f.file(bytes(&p), 0, warn));
    assert_eq!(r.unwrap(), b"ASCII text");
}

#[test]
fn set_flags_persists() {
    let mut f = default_finfo(FILEINFO_NONE);
    f.set_flags(FILEINFO_MIME_TYPE);
    let (r, _) = warnings(|warn| f.buffer(b"<?php echo 1;\n", 0, warn));
    assert_eq!(r.unwrap(), b"text/x-php");
    let (r, _) = warnings(|warn| f.buffer(b"<?php echo 1;\n", FILEINFO_MIME_ENCODING, warn));
    assert_eq!(r.unwrap(), b"us-ascii");
    let (r, _) = warnings(|warn| f.buffer(b"<?php echo 1;\n", 0, warn));
    assert_eq!(r.unwrap(), b"text/x-php");
}

#[test]
fn compress_flag_is_ignored() {
    let gz = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03\x4b\x4c\x4a\x06\x00\xc2\x41\x24\x35\x03\x00\x00\x00";
    let mut f = default_finfo(0x4);
    let (r, _) = warnings(|warn| f.buffer(gz, FILEINFO_MIME_TYPE | 0x4, warn));
    assert_eq!(r.unwrap(), b"application/gzip");
}

#[test]
fn file_directory_and_missing() {
    let mut f = default_finfo(FILEINFO_NONE);
    // finfo_file_001.phpt
    let (r, w) = warnings(|warn| f.file(b".", 0, warn));
    assert_eq!(r.unwrap(), b"directory");
    assert!(w.is_empty());
    let (r, w) = warnings(|warn| f.file(b"&", 0, warn));
    assert!(r.is_none());
    assert_eq!(
        messages(&w),
        ["(&) Failed to open stream: No such file or directory"]
    );
    let (r, _) = warnings(|warn| f.file(b"/dev/null", 0, warn));
    assert_eq!(r.unwrap(), b"empty");
}

#[test]
fn file_contents() {
    let p = scratch("bug69320.txt");
    std::fs::write(&p, "foo").unwrap();
    let mut f = default_finfo(FILEINFO_MIME_TYPE);
    let (r, _) = warnings(|warn| f.file(bytes(&p), 0, warn));
    assert_eq!(r.unwrap(), b"text/plain");
    let php = scratch("x.php");
    std::fs::write(&php, "<?php\nvar_dump(1);\n").unwrap();
    let (r, _) = warnings(|warn| f.file(bytes(&php), FILEINFO_MIME, warn));
    assert_eq!(r.unwrap(), b"text/x-php; charset=us-ascii");
    let (r, _) = warnings(|warn| f.file(bytes(&php), FILEINFO_CONTINUE, warn));
    assert_eq!(r.unwrap(), b"PHP script, ASCII text");
    // The descriptor is examined from its offset, which is left alone.
    let mut file = File::open(&php).unwrap();
    let (r, _) = warnings(|warn| f.file_handle(&file, 0, warn));
    assert_eq!(r.unwrap(), b"text/x-php");
    let mut two = [0u8; 2];
    file.read_exact(&mut two).unwrap();
    let (r, _) = warnings(|warn| f.file_handle(&file, 0, warn));
    assert_eq!(r.unwrap(), b"text/plain");
    assert_eq!(file.stream_position().unwrap(), 2);
}

#[test]
fn reader_reads_from_current_position() {
    let mut f = default_finfo(FILEINFO_MIME_TYPE);
    let mut c = Cursor::new(b"xxGIF89a\x01\x00\x01\x00\x00\x00".to_vec());
    c.set_position(2);
    let (r, _) = warnings(|warn| f.reader(&mut c, 0, warn));
    assert_eq!(r.unwrap(), b"image/gif");
}

#[test]
fn open_missing_database() {
    // finfo_open_001.phpt / finfo_open_error.phpt
    let (r, w) = warnings(|warn| Finfo::open(FILEINFO_MIME, Some(b"/foo/bar/inexistent"), warn));
    assert!(r.is_none());
    assert_eq!(
        messages(&w),
        [
            "(/foo/bar/inexistent) Failed to open stream: No such file or directory",
            "(/foo/bar/inexistent) Failed to open stream: No such file or directory",
            "Failed to load magic database at \"/foo/bar/inexistent\"",
        ]
    );
    // Empty means "default".
    let (r, w) = warnings(|warn| Finfo::open(FILEINFO_MIME, Some(b""), warn));
    assert!(r.is_some() && w.is_empty());
}

fn fixture(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../modules/php-src/ext/fileinfo/tests")
        .join(name);
    p.exists().then_some(p)
}

#[test]
fn open_text_database_with_check_flag() {
    let Some(magic) = fixture("magic") else {
        return;
    };
    let (r, w) = warnings(|warn| Finfo::open(0x7fff_ffff_ffff_fffe, Some(bytes(&magic)), warn));
    assert!(r.is_some(), "{w:?}");
    assert_eq!(
        messages(&w),
        [format!("using regular magic file `{}'", magic.display())]
    );
    let (r, w) = warnings(|warn| Finfo::open(FILEINFO_MIME, Some(bytes(&magic)), warn));
    assert!(w.is_empty());
    let mut f = r.unwrap();
    // finfo_set_flags_basic.phpt
    f.set_flags(FILEINFO_NONE);
    f.set_flags(FILEINFO_SYMLINK);
}

#[test]
fn open_invalid_text_databases() {
    // bug77961.phpt
    if let Some(magic) = fixture("bug77961.magic") {
        let (r, w) = warnings(|warn| Finfo::open(0, Some(bytes(&magic)), warn));
        assert!(r.is_none());
        assert_eq!(
            messages(&w),
            [
                "Expected numeric type got `indirect'".to_owned(),
                format!("Failed to load magic database at \"{}\"", magic.display()),
            ]
        );
    }
    // bug71527.phpt
    if let Some(magic) = fixture("bug71527.magic") {
        let (r, w) = warnings(|warn| Finfo::open(0, Some(bytes(&magic)), warn));
        assert!(r.is_none());
        assert_eq!(
            messages(&w),
            [format!(
                "Failed to load magic database at \"{}\"",
                magic.display()
            )]
        );
    }
}

#[test]
fn open_directory_database() {
    // bug61964.phpt
    let Some(magic) = fixture("magic") else {
        return;
    };
    let dir = scratch("bug61964");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(&magic, dir.join("magic.copy")).unwrap();
    std::fs::copy(&magic, dir.join("magic.copy2")).unwrap();
    let (r, w) = warnings(|warn| Finfo::open(0, Some(bytes(&dir)), warn));
    assert!(r.is_some(), "{w:?}");
    assert!(w.is_empty(), "{w:?}");
    std::fs::write(dir.join("test1.txt"), "string\n> Core\n> Me").unwrap();
    std::fs::write(dir.join("test2.txt"), "a\nb\n").unwrap();
    std::fs::create_dir_all(dir.join("test-inner-folder")).unwrap();
    let (r, w) = warnings(|warn| Finfo::open(0, Some(bytes(&dir)), warn));
    assert!(r.is_none());
    assert_eq!(
        messages(&w),
        [
            "offset `string' invalid".to_owned(),
            "offset ` Core' invalid".to_owned(),
            "offset ` Me' invalid".to_owned(),
            "offset `a' invalid".to_owned(),
            "offset `b' invalid".to_owned(),
            format!("Failed to load magic database at \"{}\"", dir.display()),
        ]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn indirect_recursion_limit() {
    // cve-2014-1943.phpt
    let fm = scratch("cve-2014-1943.magic");
    std::fs::write(
        &fm,
        "0           byte        x\n>(1.b)      indirect    x\n",
    )
    .unwrap();
    let fd = scratch("cve-2014-1943.data");
    std::fs::write(&fd, vec![1u8; 250_000]).unwrap();
    let (r, _) = warnings(|warn| Finfo::open(0, Some(bytes(&fm)), warn));
    let mut f = r.unwrap();
    let (r, w) = warnings(|warn| f.file(bytes(&fd), 0, warn));
    assert!(r.is_none(), "{r:?} {w:?}");
    assert_eq!(
        messages(&w),
        ["Failed identify data 0:indirect count (50) exceeded"]
    );
}

#[test]
fn argument_checks() {
    assert_eq!(
        check_filename(CallStyle::Function, b"\0"),
        Err(Thrown::ValueError(
            "finfo_file(): Argument #2 ($filename) must not contain any null bytes".into()
        ))
    );
    assert_eq!(
        check_filename(CallStyle::Method, b""),
        Err(Thrown::ValueError(
            "finfo::file(): Argument #1 ($filename) must not be empty".into()
        ))
    );
    assert_eq!(check_filename(CallStyle::Method, b"x"), Ok(()));
    assert_eq!(
        check_magic_database(CallStyle::Function, b"\0"),
        Err(Thrown::ValueError(
            "finfo_open(): Argument #2 ($magic_database) must not contain any null bytes".into()
        ))
    );
    assert_eq!(
        check_mime_content_type_filename(b""),
        Err(Thrown::ValueError(
            "mime_content_type(): Argument #1 ($filename) must not be empty".into()
        ))
    );
    assert_eq!(
        check_mime_content_type_filename(b"\0"),
        Err(Thrown::TypeError(
            "mime_content_type(): Argument #1 ($filename) must not contain any null bytes".into()
        ))
    );
    assert_eq!(
        mime_content_type_type_error("stdClass"),
        Thrown::TypeError(
            "mime_content_type(): Argument #1 ($filename) must be of type resource|string, stdClass given".into()
        )
    );
}

#[test]
fn mime_content_type_variants() {
    let php = scratch("gh17039.php");
    std::fs::write(&php, "<?php\nclass Foo {}\n").unwrap();
    let (r, _) = warnings(|warn| mime_content_type(bytes(&php), warn));
    assert_eq!(r.unwrap(), b"text/x-php");
    let (r, _) = warnings(|warn| mime_content_type(b".", warn));
    assert_eq!(r.unwrap(), b"directory");
    let (r, w) = warnings(|warn| mime_content_type(b"foo/inexistent", warn));
    assert!(r.is_none());
    assert_eq!(
        messages(&w),
        ["(foo/inexistent) Failed to open stream: No such file or directory"]
    );
    let mut file = File::open(&php).unwrap();
    file.seek(SeekFrom::Start(3)).unwrap();
    let (r, _) = warnings(|warn| mime_content_type_stream(&mut file, warn));
    assert_eq!(r.unwrap(), b"text/x-php");
    assert_eq!(file.stream_position().unwrap(), 3);
}

#[test]
fn helpers() {
    assert_eq!(
        expand_filepath(b"a/./b/../c", b"/x/y").unwrap(),
        b"/x/y/a/c"
    );
    assert_eq!(expand_filepath(b"/..", b"/x").unwrap(), b"/");
    assert!(expand_filepath(&[b'a'; 5000], b"/").is_none());
    assert!(libmagic_version().starts_with("5."));
    assert_eq!(
        buffer_context_deprecation(),
        "The $context parameter has no effect for finfo_buffer()"
    );
    let e = std::io::Error::from_raw_os_error(2);
    assert_eq!(strerror(&e), "No such file or directory");
}
