//! File function tests; expected results and messages were taken from
//! `/usr/bin/php` (message prefixes follow master: no docref parameters).

use super::*;
use crate::diag;
use crate::filestat::{php_stat, FsType, StatResult};
use crate::test_util::TempDir;

fn msgs() -> Vec<String> {
    diag::take().iter().map(diag::Diagnostic::text).collect()
}

fn p(dir: &TempDir, name: &str) -> Vec<u8> {
    dir.file(name)
}

#[test]
fn open_failures() {
    let d = TempDir::new();
    std::fs::write(crate::util::os_path(&p(&d, "f")), b"x").unwrap();
    let _ = diag::take();
    assert!(open::fopen(&p(&d, "nope"), "r", false, None).is_none());
    assert!(open::fopen(&p(&d, "f"), "x", false, None).is_none());
    assert!(open::fopen(&p(&d, "f"), "Q", false, None).is_none());
    let mut glob = b"glob://".to_vec();
    glob.extend_from_slice(&p(&d, "*"));
    assert!(open::fopen(&glob, "r", false, None).is_none());
    assert!(open::fopen(b"foo://bar", "r", false, None).is_none());
    assert_eq!(
        msgs(),
        [
            "Failed to open stream: No such file or directory",
            "Failed to open stream: File exists",
            "Failed to open stream: `Q' is not a valid mode for fopen",
            "Failed to open stream: wrapper does not support stream open",
            "Unable to find the wrapper \"foo\" - did you forget to enable it when you configured PHP?",
            "Failed to open stream: No such file or directory",
        ]
    );
}

#[test]
fn get_contents_and_lines() {
    let d = TempDir::new();
    let f = p(&d, "f");
    std::fs::write(crate::util::os_path(&f), b"a\n\nb\r\nc").unwrap();
    assert_eq!(
        file_get_contents(&f, false, None, 2, Some(3))
            .unwrap()
            .unwrap(),
        b"\nb\r"
    );
    assert_eq!(
        file_get_contents(&f, false, None, -2, None)
            .unwrap()
            .unwrap(),
        b"\nc"
    );
    assert_eq!(
        file_get_contents(&f, false, None, 100, None)
            .unwrap()
            .unwrap(),
        b""
    );
    assert!(file_get_contents(&f, false, None, 0, Some(-1)).is_err());
    let lines = file(&f, 0, None).unwrap().unwrap();
    assert_eq!(
        lines,
        vec![
            b"a\n".to_vec(),
            b"\n".to_vec(),
            b"b\r\n".to_vec(),
            b"c".to_vec()
        ]
    );
    let lines = file(&f, FILE_IGNORE_NEW_LINES, None).unwrap().unwrap();
    assert_eq!(
        lines,
        vec![b"a".to_vec(), b"".to_vec(), b"b".to_vec(), b"c".to_vec()]
    );
    let lines = file(&f, FILE_IGNORE_NEW_LINES | FILE_SKIP_EMPTY_LINES, None)
        .unwrap()
        .unwrap();
    assert_eq!(lines, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
    assert!(file(&f, 64, None).is_err());
    assert!(split_lines(b"", b'\n', true, false).is_empty());
    assert_eq!(
        split_lines(b"\n\n", b'\n', false, true),
        Vec::<Vec<u8>>::new()
    );
}

#[test]
fn split_lines_reserving_matches_split_lines() {
    let inputs: [&[u8]; 6] = [b"", b"a", b"a\nb", b"a\r\n\nb\n", b"\n\n", b"x\ny\n\n"];
    for buf in inputs {
        for (inc, skip) in [(true, false), (false, false), (false, true)] {
            assert_eq!(
                split_lines_reserving(buf, b'\n', inc, skip),
                Some(split_lines(buf, b'\n', inc, skip)),
                "{buf:?} {inc} {skip}"
            );
        }
    }
    assert_eq!(zend_string_size(0), 32);
    assert_eq!(zend_string_size(7), 32);
    assert_eq!(zend_string_size(8), 40);
}

#[test]
fn directories_and_renames() {
    let d = TempDir::new();
    let f = p(&d, "f");
    std::fs::write(crate::util::os_path(&f), b"data").unwrap();
    let _ = diag::take();
    assert!(mkdir(&p(&d, "x/y/z"), 0o777, true, None));
    assert!(!mkdir(&p(&d, "x"), 0o777, false, None));
    assert!(!mkdir(&p(&d, "q/r"), 0o777, false, None));
    assert!(!rmdir(&p(&d, "x"), None));
    assert!(!unlink(&p(&d, "nope"), None));
    assert!(!rename(&p(&d, "nope"), &p(&d, "n2"), None));
    assert!(!rename(&f, b"data:,x", None));
    assert!(!copy(&p(&d, "x"), &p(&d, "c"), None));
    assert!(!copy(&f, &p(&d, "x"), None));
    assert!(!copy(&p(&d, "nope"), &p(&d, "c"), None));
    assert!(!copy(&f, &f, None));
    assert!(!unlink(b"data://x", None));
    assert!(!rename(b"data:,a", b"data:,b", None));
    assert_eq!(
        msgs(),
        [
            "File exists",
            "No such file or directory",
            "Directory not empty",
            "No such file or directory",
            "No such file or directory",
            "Cannot rename a file across wrapper types",
            "The first argument to copy() function cannot be a directory",
            "The second argument to copy() function cannot be a directory",
            "Failed to open stream: No such file or directory",
            "RFC2397 does not allow unlinking",
            "RFC2397 wrapper does not support renaming",
        ]
    );
    assert!(copy(&f, &p(&d, "c"), None));
    assert!(rename(&p(&d, "c"), &p(&d, "c2"), None));
    assert_eq!(
        std::fs::read(crate::util::os_path(&p(&d, "c2"))).unwrap(),
        b"data"
    );
    assert!(unlink(&p(&d, "c2"), None));
    let names = open::scandir(&p(&d, "x"), open::ScandirSort::Ascending, None).unwrap();
    assert_eq!(names, vec![b".".to_vec(), b"..".to_vec(), b"y".to_vec()]);
    let names = open::scandir(&p(&d, "x"), open::ScandirSort::Descending, None).unwrap();
    assert_eq!(names[0], b"y");
    assert!(open::scandir(&p(&d, "nope"), open::ScandirSort::None, None).is_none());
    assert_eq!(
        msgs(),
        ["Failed to open directory: No such file or directory"]
    );
    // recursive mkdir of an existing directory fails with EEXIST
    assert!(!mkdir(&p(&d, "x/y"), 0o777, true, None));
    assert_eq!(msgs(), ["File exists"]);
    assert!(mkdir(&p(&d, "x/y/./w/../v"), 0o777, true, None));
}

#[test]
fn put_contents_flags() {
    let d = TempDir::new();
    let f = p(&d, "f2");
    let chunks = vec![b"a".to_vec(), b"bc".to_vec(), Vec::new()];
    assert_eq!(
        file_put_contents(&f, PutData::Chunks(&chunks), 0, None),
        Some(3)
    );
    assert_eq!(
        file_put_contents(&f, PutData::Bytes(b"x"), FILE_APPEND, None),
        Some(1)
    );
    assert_eq!(std::fs::read(crate::util::os_path(&f)).unwrap(), b"abcx");
    let _ = diag::take();
    assert_eq!(
        file_put_contents(b"data://x", PutData::Bytes(b"x"), LOCK_EX, None),
        None
    );
    assert_eq!(
        msgs(),
        ["Exclusive locks may only be set for regular files"]
    );
    assert_eq!(
        file_put_contents(&f, PutData::Bytes(b"zz"), LOCK_EX, None),
        Some(2)
    );
    assert_eq!(std::fs::read(crate::util::os_path(&f)).unwrap(), b"zz");
    let mut src =
        crate::memory::memory_open(crate::mode::TempMode::ReadOnly, b"from stream".to_vec());
    assert_eq!(
        file_put_contents(&f, PutData::Stream(&mut src), 0, None),
        Some(11)
    );
    assert_eq!(file_put_contents(&f, PutData::Invalid, 0, None), None);
}

#[test]
fn stat_family_and_cache() {
    let d = TempDir::new();
    let f = p(&d, "f");
    std::fs::write(crate::util::os_path(&f), b"abc").unwrap();
    let _ = diag::take();
    assert_eq!(
        php_stat(&p(&d, "nope"), FsType::Size),
        StatResult::Bool(false)
    );
    assert_eq!(
        php_stat(&p(&d, "nope"), FsType::Lstat),
        StatResult::Bool(false)
    );
    let mut expect = b"stat failed for ".to_vec();
    expect.extend_from_slice(&p(&d, "nope"));
    let got = diag::take();
    assert_eq!(got[0].message, expect);
    assert!(got[1].text().starts_with("Lstat failed for "));
    assert_eq!(php_stat(&f, FsType::Type), StatResult::Str("file"));
    assert_eq!(
        php_stat(&p(&d, "nope"), FsType::IsFile),
        StatResult::Bool(false)
    );
    assert_eq!(php_stat(&d.path(), FsType::IsDir), StatResult::Bool(true));
    assert_eq!(php_stat(&f, FsType::Size), StatResult::Int(3));
    // cached until a write operation clears it
    std::fs::write(crate::util::os_path(&f), b"abcdef").unwrap();
    assert_eq!(php_stat(&f, FsType::Size), StatResult::Int(3));
    crate::filestat::clearstatcache(false, None);
    assert_eq!(php_stat(&f, FsType::Size), StatResult::Int(6));
    let t = p(&d, "t");
    assert_eq!(crate::filestat::touch(&t, Some(1000), Some(2000)), Ok(true));
    assert_eq!(php_stat(&t, FsType::Mtime), StatResult::Int(1000));
    assert_eq!(php_stat(&t, FsType::Atime), StatResult::Int(2000));
    assert!(crate::filestat::chmod(&t, 0o600));
    assert_eq!(php_stat(&t, FsType::Perms), StatResult::Int(0o100_600));
    assert!(crate::filestat::touch(&t, None, Some(1)).is_err());
    let StatResult::Stat(sb) = php_stat(&t, FsType::Stat) else {
        panic!()
    };
    assert_eq!(sb.size, 0);
    assert!(sb.to_value().as_array().unwrap().len() == 26);
}

#[test]
fn symlinks() {
    let d = TempDir::new();
    let f = p(&d, "target");
    std::fs::write(crate::util::os_path(&f), b"abc").unwrap();
    let l = p(&d, "link");
    assert!(symlink(b"target", &l));
    assert_eq!(readlink(&l).unwrap(), b"target");
    assert_eq!(php_stat(&l, FsType::IsLink), StatResult::Bool(true));
    assert_eq!(php_stat(&l, FsType::Type), StatResult::Str("link"));
    assert_eq!(realpath(&l).unwrap(), f);
    let _ = diag::take();
    assert!(!symlink(b"target", &l));
    assert_eq!(msgs(), ["File exists"]);
    assert!(link(&f, &p(&d, "hard")));
    assert!(linkinfo(&l) > 0);
}

#[test]
fn stream_level_functions() {
    let mut m = crate::memory::memory_create(crate::mode::TempMode::Default);
    assert_eq!(fwrite(&mut m, b"hello", Some(3)), Some(3));
    assert_eq!(fwrite(&mut m, b"hello", Some(0)), Some(0));
    assert_eq!(ftell(&m), Some(3));
    assert!(fgets(&mut m, Some(0)).is_err());
    assert!(fread(&mut m, 0).is_err());
    assert_eq!(
        stream_get_contents(&mut m, None, 1).unwrap().unwrap(),
        b"el"
    );
    assert!(stream_get_contents(&mut m, Some(-2), -1).is_err());
    assert_eq!(ftruncate(&mut m, 1), Ok(true));
    assert!(ftruncate(&mut m, -1).is_err());
    assert_eq!(stream_set_chunk_size(&mut m, 10), Ok(8192));
    assert!(stream_set_chunk_size(&mut m, 0).is_err());
    let mut out = crate::stream::Stream::new(Box::new(crate::php_wrapper::OutputOps), "wb");
    let _ = diag::take();
    assert_eq!(ftruncate(&mut out, 0), Ok(false));
    assert_eq!(msgs(), ["Can't truncate this stream!"]);
    assert!(!fsync(&mut out, false));
    assert_eq!(msgs(), ["Can't fsync this stream!"]);
    assert_eq!(
        flock(&mut m, 0).unwrap_err().message,
        "($operation) must be one of LOCK_SH, LOCK_EX, or LOCK_UN"
    );
    assert!(stream_is_local(&m) || m.wrapper.is_none());
    assert!(url_is_local(b"/tmp"));
    assert!(!url_is_local(b"data:,x"));
}

#[test]
fn flock_on_files() {
    let d = TempDir::new();
    let f = p(&d, "lock");
    std::fs::write(crate::util::os_path(&f), b"").unwrap();
    let mut a = open::fopen(&f, "r", false, None).unwrap();
    let mut b = open::fopen(&f, "r", false, None).unwrap();
    assert_eq!(flock(&mut a, LOCK_EX), Ok((true, false)));
    assert_eq!(flock(&mut b, LOCK_EX | LOCK_NB), Ok((false, true)));
    assert_eq!(flock(&mut a, LOCK_UN), Ok((true, false)));
    assert_eq!(flock(&mut b, LOCK_SH), Ok((true, false)));
    assert!(b.supports_lock());
}

#[test]
#[allow(clippy::naive_bytecount)]
fn fgetcsv_hook() {
    let mut m =
        crate::memory::memory_open(crate::mode::TempMode::ReadOnly, b"a,\"b\nc\"\nd\n".to_vec());
    let r = fgetcsv(&mut m, None, |first, more| {
        let mut rec = first;
        if rec.iter().filter(|&&c| c == b'"').count() % 2 == 1 {
            // (a naive byte count is fine in a test)
            rec.extend(more().unwrap());
        }
        rec
    })
    .unwrap()
    .unwrap();
    assert_eq!(r, b"a,\"b\nc\"\n");
    assert!(fgetcsv(&mut m, Some(-1), |l, _| l).is_err());
}

#[test]
fn readfile_and_passthru() {
    let mut out = Vec::new();
    let n = readfile(b"data:,hello%20there", false, None, &mut |b| {
        out.extend_from_slice(b);
    });
    assert_eq!(n, Some(11));
    assert_eq!(out, b"hello there");
}

#[test]
fn fnmatch_and_umask() {
    assert!(fnmatch(b"*.txt", b"a.txt", 0));
    let old = umask(None);
    assert_eq!(umask(Some(0o077)), old);
    assert_eq!(umask(Some(old)), 0o077);
}

#[test]
fn filter_attach_by_mode() {
    let mut m = crate::memory::memory_create(crate::mode::TempMode::Default);
    // "w+b": both chains
    let id = stream_filter_attach(&mut m, "string.toupper", 0, None, true).unwrap();
    assert_eq!(m.read_filters.len(), 1);
    assert_eq!(m.write_filters.len(), 1);
    assert_eq!(m.filter_chain_of(id), Some(crate::filter::ChainKind::Write));
    m.write(b"abc").unwrap();
    m.rewind();
    assert_eq!(m.copy_to_mem(None).unwrap(), b"ABC");
    assert!(stream_filter_remove(&mut m, id));
    let _ = diag::take();
    assert!(stream_filter_attach(&mut m, "nonexistent", STREAM_FILTER_READ, None, false).is_none());
    assert_eq!(msgs(), ["Unable to locate filter \"nonexistent\""]);
}

#[test]
fn auto_detect_mac_line_endings() {
    crate::state::update_settings(|s| s.auto_detect_line_endings = true);
    let mut m = crate::memory::memory_open(crate::mode::TempMode::ReadOnly, b"a\rb\rc".to_vec());
    crate::state::update_settings(|s| s.auto_detect_line_endings = false);
    assert_eq!(m.get_line(None).unwrap(), b"a\r");
    assert_eq!(m.get_line(None).unwrap(), b"b\r");
    assert_eq!(m.get_line(None).unwrap(), b"c");
}
