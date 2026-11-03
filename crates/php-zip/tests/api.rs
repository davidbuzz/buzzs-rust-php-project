//! Direct tests of the Rust API (lifecycle, streams, tables) that the
//! scenario transcripts do not cover.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use php_runtime::{ErrorLevel, RtResult, RuntimeContext, Value};
use php_streams::wrapper::Wrapper;
use php_zip::{ExtractFiles, OpenResult, RegexMatcher, ZipArchive, ZipHost};

#[derive(Default)]
struct Host {
    calls: Rc<RefCell<Vec<String>>>,
}

impl RuntimeContext for Host {
    fn diagnostic(&mut self, _level: ErrorLevel, _message: &str) -> RtResult<()> {
        Ok(())
    }
}

impl ZipHost for Host {
    fn call(&mut self, callable: &Value, args: &[Value]) -> RtResult<Value> {
        self.calls
            .borrow_mut()
            .push(format!("{}:{}", callable.type_name(), args.len()));
        Ok(Value::Int(0))
    }

    fn compile_regex(&mut self, _regex: &[u8]) -> Option<RegexMatcher> {
        Some(Box::new(|name: &[u8]| name.ends_with(b".txt")))
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("php-zip-api-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn bytes(p: &std::path::Path) -> Vec<u8> {
    p.to_str().unwrap().as_bytes().to_vec()
}

#[test]
fn drop_without_destroy_writes_the_archive() {
    let d = tmp("drop");
    let file = d.join("a.zip");
    let mut host = Host::default();
    {
        let z = ZipArchive::new();
        assert_eq!(
            z.open(&mut host, &bytes(&file), 1).unwrap(),
            OpenResult::Opened
        );
        assert!(z.add_from_string(b"x.txt", b"hello", 8192).unwrap());
        assert!(z
            .register_progress_callback(0.0, Value::from("cb"))
            .unwrap());
    }
    // Without a host the callback is skipped (EG(active) is false).
    assert!(host.calls.borrow().is_empty());
    let z = ZipArchive::new();
    assert_eq!(
        z.open(&mut host, &bytes(&file), 0).unwrap(),
        OpenResult::Opened
    );
    assert_eq!(
        z.get_from_name(b"x.txt", 0, 0).unwrap().as_deref(),
        Some(&b"hello"[..])
    );
    assert_eq!(z.count().unwrap(), 1);
    z.destroy(&mut host).unwrap();
    assert!(!z.is_open());
    // Destroying twice is harmless.
    z.destroy(&mut host).unwrap();
    let _ = fs::remove_dir_all(d);
}

#[test]
fn callbacks_run_through_the_host() {
    let d = tmp("cb");
    let file = d.join("a.zip");
    let mut host = Host::default();
    let z = ZipArchive::new();
    z.open(&mut host, &bytes(&file), 1).unwrap();
    z.add_from_string(b"x.txt", b"hello", 8192).unwrap();
    z.register_progress_callback(0.5, Value::from("p")).unwrap();
    z.register_cancel_callback(Value::from("c")).unwrap();
    assert!(z.close(&mut host).unwrap());
    let calls = host.calls.borrow().clone();
    assert!(calls.contains(&"string:1".to_string()), "{calls:?}");
    assert!(calls.contains(&"string:0".to_string()), "{calls:?}");
    let _ = fs::remove_dir_all(d);
}

#[test]
fn streams_and_wrapper() {
    let fix = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../modules/php-src/ext/zip/tests/test_with_comment.zip");
    let mut host = Host::default();
    let z = ZipArchive::new();
    z.open(&mut host, &bytes(&fix), consts_rdonly()).unwrap();
    let mut s = z.get_stream(b"foo").unwrap().unwrap();
    assert_eq!(s.ops_label(), "zip");
    assert_eq!(s.orig_path.as_deref(), Some(&b"foo"[..]));
    assert!(!s.is_seekable() || s.seek(0, 0) == -1);
    // The stream keeps the archive alive after the object lets go of it.
    z.destroy(&mut host).unwrap();
    assert_eq!(s.copy_to_mem(None).as_deref(), Some(&b"foo\n\n"[..]));
    s.close();

    let w = php_zip::ZipWrapper;
    assert_eq!(w.label(), "zip wrapper");
    let mut opened = None;
    let path = [b"zip://".as_slice(), &bytes(&fix), b"#bar"].concat();
    let mut s = w.open(&path, "rb", 0, &mut opened, None).unwrap();
    assert_eq!(opened.as_deref(), Some(&path[6..]));
    let st = s.stat().unwrap();
    assert_eq!(st.size, 4);
    assert_eq!(st.mode, 0o100_000);
    assert_eq!(s.copy_to_mem(None).as_deref(), Some(&b"bar\n"[..]));
    s.close();
    assert!(w.open(&path, "wb", 0, &mut None, None).is_err());
    assert!(php_zip::stream::stat_url(b"zip:///nonexistent.zip#x").is_none());
}

fn consts_rdonly() -> i64 {
    php_zip::consts::RDONLY
}

#[test]
fn properties_and_errors() {
    let z = ZipArchive::new();
    assert!(matches!(z.read_property(b"lastId"), Some(Value::Int(-1))));
    assert!(z.read_property(b"bogus").is_none());
    assert!(ZipArchive::is_virtual_property(b"comment"));
    assert!(!ZipArchive::is_virtual_property(b"custom"));
    let names: Vec<&str> = z.properties().iter().map(|(n, _)| *n).collect();
    assert_eq!(names, php_zip::PROPERTY_NAMES);
    assert_eq!(
        ZipArchive::serialize("myZip").to_string(),
        "Exception: Serialization of 'myZip' is not allowed, override __serialize() and __unserialize() to implement it"
    );
    assert_eq!(
        ZipArchive::unserialize("ZipArchive").message(),
        Some("Unserialization of 'ZipArchive' is not allowed, override __serialize() and __unserialize() to implement it")
    );
    assert_eq!(php_zip::archive::error_message(0, 0), "No error");
    assert_eq!(
        php_zip::archive::error_message(11, 2),
        "Can't open file: No such file or directory"
    );
    assert_eq!(z.get_status_string(), "No error");
    assert!(
        !z.extract_to(b"", ExtractFiles::All).unwrap(),
        "empty target is checked before the object"
    );
}

#[test]
fn stat_array_shape() {
    let fix = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../modules/php-src/ext/zip/tests/test.zip");
    let mut host = Host::default();
    let z = ZipArchive::new();
    z.open(&mut host, &bytes(&fix), 0).unwrap();
    let st = z.stat_index(0, 0).unwrap().unwrap();
    let arr = st.to_array();
    let keys: Vec<String> = arr
        .keys()
        .map(|k| String::from_utf8_lossy(&k.to_value().to_string_lossy_vec()).to_string())
        .collect();
    assert_eq!(
        keys,
        [
            "name",
            "index",
            "crc",
            "size",
            "mtime",
            "comp_size",
            "comp_method",
            "encryption_method"
        ]
    );
    assert!(matches!(arr.get_str(b"crc"), Some(Value::Int(77_771_753))));
    assert_eq!(
        z.get_external_attributes_index(0, 0).unwrap(),
        Some((3, 0o100_664 << 16)),
        "unix -rw-rw-r--"
    );
    let files = z.add_pattern(&mut host, b"/x/", b"/nonexistent-dir-for-php-zip", None);
    assert_eq!(files.unwrap(), None);
}

trait Lossy {
    fn to_string_lossy_vec(&self) -> Vec<u8>;
}

impl Lossy for Value {
    fn to_string_lossy_vec(&self) -> Vec<u8> {
        match self {
            Value::Str(s) => s.as_bytes().to_vec(),
            Value::Int(i) => i.to_string().into_bytes(),
            _ => Vec::new(),
        }
    }
}

#[test]
fn info_and_version() {
    let rows = php_zip::consts::info_rows();
    assert_eq!(rows[1].0, "Libzip version");
    assert_eq!(rows[1].1, php_zip::LIBZIP_HEADERS_VERSION);
    assert!(rows
        .iter()
        .any(|(k, v)| *k == "AES-256 encryption" && v == "Yes"));
    assert!(rows
        .iter()
        .any(|(k, v)| *k == "ZSTD compression" && v == "No"));
}
