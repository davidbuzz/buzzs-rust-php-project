// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Stream-level tests; expected values were taken from `/usr/bin/php8.5`.

use super::*;
use crate::memory::{memory_create, temp_create};
use crate::mode::TempMode;
use crate::open;
use crate::test_util::TempDir;

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn memory_stream_sequence() {
    let mut m = memory_create(TempMode::Default);
    assert_eq!(m.write(b"line1\nline2\r\nline3"), Ok(18));
    assert_eq!(m.tell(), 18);
    assert!(!m.eof());
    m.rewind();
    assert_eq!(m.get_line(None).unwrap(), b"line1\n");
    assert_eq!(m.get_line(Some(3)).unwrap(), b"li");
    assert_eq!(m.get_line(Some(1024)).unwrap(), b"ne2\r\n");
    assert_eq!(m.get_line(None).unwrap(), b"line3");
    assert!(m.eof());
    assert_eq!(m.get_line(None), None);
    // seeking before the start fails and ftell() then reports false
    assert_eq!(m.seek(-100, SEEK_CUR), -1);
    assert_eq!(m.tell(), -1);
    assert_eq!(m.seek(2, SEEK_SET), 0);
    assert_eq!(m.tell(), 2);
    assert_eq!(m.read_vec(3).unwrap(), b"ne1");
    assert_eq!(m.getc(), Some(b'\n'));
    assert_eq!(m.seek(5, SEEK_END), 0);
    assert_eq!(m.tell(), 23);
    assert_eq!(m.write(b"X"), Ok(1));
    m.seek(0, SEEK_SET);
    assert_eq!(m.copy_to_mem(None).unwrap().len(), 24);
    m.rewind();
    assert_eq!(m.get_record(100, b"\r\n").unwrap(), b"line1\nline2");
    assert_eq!(m.get_record(3, b"").unwrap(), b"lin");
    assert_eq!(m.get_record(100, b"zz").unwrap(), b"e3\0\0\0\0\0X");
    assert_eq!(m.truncate(3), OPT_OK);
    m.seek(0, SEEK_SET);
    assert_eq!(m.copy_to_mem(None).unwrap(), b"lin");
    assert_eq!(m.stat().unwrap().size, 3);
}

#[test]
fn readonly_memory_rejects_writes() {
    let mut r = memory_create(TempMode::ReadOnly);
    assert!(r.write(b"abc").is_err());
    assert_eq!(r.mode, "rb");
    let meta = r.meta_data();
    let keys: Vec<&str> = meta.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "timed_out",
            "blocked",
            "eof",
            "stream_type",
            "mode",
            "unread_bytes",
            "seekable"
        ]
    );
}

#[test]
fn temp_spills_to_file() {
    let mut t = temp_create(TempMode::Default, 4, None);
    assert_eq!(t.write(b"abcdef"), Ok(6));
    assert!(t
        .ops_as::<crate::memory::TempOps>()
        .unwrap()
        .is_file_backed());
    t.rewind();
    assert_eq!(t.read_vec(10).unwrap(), b"abcdef");
    assert_eq!(t.ops_label(), "TEMP");
    let meta = t.meta_data();
    assert_eq!(meta[0].0, "stream_type");
}

#[test]
fn plain_file_read_buffering() {
    let dir = TempDir::new();
    let path = dir.file("data.txt");
    let content: Vec<u8> = (0..20000u32)
        .map(|i| b'a' + u8::try_from(i % 26).unwrap())
        .collect();
    std::fs::write(crate::util::os_path(&path), &content).unwrap();
    let mut f = open::fopen(&path, "r", false, None).unwrap();
    // plain files read greedily: one fread returns everything requested
    let got = f.read_vec(15000).unwrap();
    assert_eq!(got.len(), 15000);
    assert_eq!(f.tell(), 15000);
    assert!(!f.eof());
    let rest = f.copy_to_mem(None).unwrap();
    assert_eq!(rest.len(), 5000);
    assert!(f.eof());
    let meta = f.meta_data();
    assert_eq!(
        meta[3],
        (
            "wrapper_type".to_string(),
            crate::value::Value::str("plainfile")
        )
    );
    assert_eq!(
        meta[4],
        ("stream_type".to_string(), crate::value::Value::str("STDIO"))
    );
    assert_eq!(
        meta.last().unwrap().1,
        crate::value::Value::Str(path.clone())
    );
}

#[test]
fn feof_after_exact_read() {
    let dir = TempDir::new();
    let path = dir.file("x");
    std::fs::write(crate::util::os_path(&path), b"abc").unwrap();
    let mut f = open::fopen(&path, "r", false, None).unwrap();
    assert_eq!(f.read_vec(3).unwrap(), b"abc");
    // PHP: feof() is false until a read hits the end
    assert!(!f.eof());
    assert_eq!(f.read_vec(1).unwrap(), b"");
    assert!(f.eof());
}

#[test]
fn append_mode_position_and_write_through() {
    let dir = TempDir::new();
    let path = dir.file("a");
    std::fs::write(crate::util::os_path(&path), b"12345").unwrap();
    let mut f = open::fopen(&path, "a+", false, None).unwrap();
    // PHP reports 0 before the first write and counts written bytes after
    assert_eq!(f.tell(), 0);
    f.write(b"67").unwrap();
    assert_eq!(f.tell(), 2);
    assert_eq!(
        std::fs::read(crate::util::os_path(&path)).unwrap(),
        b"1234567"
    );
    f.rewind();
    assert_eq!(f.read_vec(100).unwrap(), b"1234567");
}

#[test]
fn read_filter_and_remove() {
    let mut m = memory_create(TempMode::Default);
    m.write(b"hello world").unwrap();
    m.rewind();
    let f = crate::wrapper::create_filter("string.toupper", None, false).unwrap();
    let id = m.append_filter(crate::filter::ChainKind::Read, f).unwrap();
    assert_eq!(m.read_vec(5).unwrap(), b"HELLO");
    assert!(m.remove_filter(id));
    // data already in the read buffer stays filtered
    assert_eq!(m.copy_to_mem(None).unwrap(), b" WORLD");
}

#[test]
fn write_filter_chain() {
    let mut m = memory_create(TempMode::Default);
    let f = crate::wrapper::create_filter("string.rot13", None, false).unwrap();
    m.append_filter(crate::filter::ChainKind::Write, f);
    let f = crate::wrapper::create_filter("convert.base64-encode", None, false).unwrap();
    let id = m.append_filter(crate::filter::ChainKind::Write, f).unwrap();
    m.write(b"abcd").unwrap();
    assert!(m.remove_filter(id));
    m.rewind();
    // rot13("abcd") = "nopq", base64 = "bm9wcQ=="
    assert_eq!(s(&m.copy_to_mem(None).unwrap()), "bm9wcQ==");
}

#[test]
fn filter_appended_after_buffered_read() {
    let dir = TempDir::new();
    let path = dir.file("f");
    std::fs::write(crate::util::os_path(&path), b"abc\ndef\n").unwrap();
    let mut f = open::fopen(&path, "r", false, None).unwrap();
    assert_eq!(f.get_line(None).unwrap(), b"abc\n");
    let filt = crate::wrapper::create_filter("string.toupper", None, false).unwrap();
    f.append_filter(crate::filter::ChainKind::Read, filt);
    assert_eq!(f.get_line(None).unwrap(), b"DEF\n");
}

#[test]
fn not_writable_notice() {
    let _ = crate::diag::take();
    let mut d = crate::open::opendir(b"/", 0, None).unwrap();
    assert!(d.write(b"x").is_err());
    let diags = crate::diag::take();
    assert_eq!(diags[0].text(), "Stream is not writable");
    assert_eq!(diags[0].severity, crate::diag::Severity::Notice);
}

#[test]
fn seek_emulation_on_unseekable() {
    let _ = crate::diag::take();
    let mut d = crate::open::opendir(b"/", 0, None).unwrap();
    assert!(d.readdir().is_some());
    assert_eq!(d.rewinddir(), 0);
    let mut out = Stream::new(Box::new(crate::php_wrapper::OutputOps), "wb");
    assert_eq!(out.seek(10, SEEK_SET), -1);
    assert_eq!(
        crate::diag::take()[0].text(),
        "Stream does not support seeking"
    );
    assert!(out.read_vec(4).is_none());
}

#[test]
fn copy_between_streams() {
    let mut a = crate::memory::memory_open(TempMode::ReadOnly, b"0123456789".to_vec());
    let mut b = memory_create(TempMode::Default);
    assert_eq!(copy_to_stream(&mut a, &mut b, Some(4)), (true, 4));
    assert_eq!(copy_to_stream(&mut a, &mut b, None), (true, 6));
    b.rewind();
    assert_eq!(b.copy_to_mem(None).unwrap(), b"0123456789");
}

#[test]
fn chunk_size_option() {
    let mut m = memory_create(TempMode::Default);
    assert_eq!(m.set_chunk_size(100), 8192);
    assert_eq!(m.set_chunk_size(5), 100);
}
