//! User-space wrapper protocol tests with a mock class (mirroring the
//! classic `VariableStream` examples from ext/standard/tests/file).

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::diag;
use crate::open;
use crate::wrapper;

#[derive(Debug, Default)]
struct Shared {
    data: Vec<u8>,
    calls: Vec<String>,
}

#[derive(Debug)]
struct VarStream {
    shared: Rc<RefCell<Shared>>,
    pos: usize,
    implement: &'static [&'static str],
    open_result: Value,
}

impl UserStreamObject for VarStream {
    fn call(&mut self, method: &str, args: &mut [Value]) -> UserCall {
        if !self.implement.contains(&method) {
            return UserCall::NotImplemented;
        }
        self.shared.borrow_mut().calls.push(match args.first() {
            Some(Value::Int(n)) => format!("{method}({n})"),
            Some(Value::Str(b)) if method == "stream_write" => format!("{method}[{}]", b.len()),
            _ => method.to_string(),
        });
        let mut sh = self.shared.borrow_mut();
        let r = match method {
            "stream_open" => {
                args[3] = Value::str("/opened/path");
                self.open_result.clone()
            }
            "stream_read" => {
                let n = usize::try_from(args[0].to_long()).unwrap();
                let end = (self.pos + n).min(sh.data.len());
                let out = sh.data[self.pos..end].to_vec();
                self.pos = end;
                Value::Str(out)
            }
            "stream_write" => {
                let Value::Str(b) = &args[0] else {
                    unreachable!()
                };
                let end = self.pos + b.len();
                if sh.data.len() < end {
                    sh.data.resize(end, 0);
                }
                sh.data[self.pos..end].copy_from_slice(b);
                self.pos = end;
                Value::Int(i64::try_from(b.len()).unwrap())
            }
            "stream_eof" => Value::Bool(self.pos >= sh.data.len()),
            "stream_tell" => Value::Int(i64::try_from(self.pos).unwrap()),
            "stream_seek" => {
                self.pos = usize::try_from(args[0].to_long()).unwrap();
                Value::Bool(true)
            }
            "stream_stat" | "url_stat" => Value::Array(vec![(
                Key::str("size"),
                Value::Int(i64::try_from(sh.data.len()).unwrap()),
            )]),
            "dir_opendir" | "unlink" | "stream_flush" => Value::Bool(true),
            "dir_readdir" => {
                if self.pos < 2 {
                    self.pos += 1;
                    Value::str(format!("entry{}", self.pos))
                } else {
                    Value::Bool(false)
                }
            }
            "stream_truncate" => Value::Int(1),
            _ => Value::Null,
        };
        UserCall::Returned(r)
    }

    fn is_callable(&self, method: &str) -> bool {
        self.implement.contains(&method)
    }
}

#[derive(Debug)]
struct Class {
    shared: Rc<RefCell<Shared>>,
    implement: &'static [&'static str],
    open_result: Value,
}

impl UserWrapperClass for Class {
    fn class_name(&self) -> String {
        "VariableStream".into()
    }

    fn instantiate(&self, _context: Option<&ContextRef>) -> Option<Box<dyn UserStreamObject>> {
        Some(Box::new(VarStream {
            shared: Rc::clone(&self.shared),
            pos: 0,
            implement: self.implement,
            open_result: self.open_result.clone(),
        }))
    }
}

const FULL: &[&str] = &[
    "stream_open",
    "stream_read",
    "stream_write",
    "stream_eof",
    "stream_tell",
    "stream_seek",
    "stream_stat",
    "url_stat",
    "dir_opendir",
    "dir_readdir",
    "unlink",
    "stream_flush",
    "stream_close",
    "stream_truncate",
];

fn setup(
    proto: &[u8],
    implement: &'static [&'static str],
    open_result: Value,
) -> Rc<RefCell<Shared>> {
    let shared = Rc::new(RefCell::new(Shared::default()));
    let class = Rc::new(Class {
        shared: Rc::clone(&shared),
        implement,
        open_result,
    });
    assert_eq!(register(proto, class, 0), RegisterResult::Ok);
    shared
}

fn msgs() -> Vec<String> {
    diag::take().iter().map(diag::Diagnostic::text).collect()
}

#[test]
fn read_write_protocol() {
    wrapper::reset_volatile();
    let shared = setup(b"var", FULL, Value::Bool(true));
    let mut opened = None;
    let mut s = open::open_wrapper(b"var://myvar", "w+", REPORT_ERRORS, &mut opened, None).unwrap();
    assert_eq!(opened.as_deref(), Some(&b"/opened/path"[..]));
    assert_eq!(s.ops_label(), "user-space");
    assert_eq!(s.write(b"hello world"), Ok(11));
    s.rewind();
    assert_eq!(s.read_vec(5).unwrap(), b"hello");
    // the first read asked the wrapper for a whole chunk
    assert!(shared
        .borrow()
        .calls
        .contains(&"stream_read(8192)".to_string()));
    assert_eq!(s.stat().unwrap().size, 11);
    assert!(s.truncate_supported());
    // writes are chunked by the chunk size for user-space streams
    s.write(&vec![b'a'; 20000]).unwrap();
    let calls = shared.borrow().calls.clone();
    let writes: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("stream_write["))
        .collect();
    assert_eq!(
        writes[writes.len() - 3..],
        [
            "stream_write[8192]",
            "stream_write[8192]",
            "stream_write[3616]"
        ]
    );
    let meta = s.meta_data();
    assert_eq!(meta[3].1, Value::str("user-space"));
    assert_eq!(s.close(), 0);
    assert!(shared.borrow().calls.last().unwrap() == "stream_close");
    wrapper::reset_volatile();
}

#[test]
fn missing_methods_warnings() {
    wrapper::reset_volatile();
    let _ = diag::take();
    setup(b"bare", &["stream_open"], Value::Bool(true));
    let mut opened = None;
    let mut s = open::open_wrapper(b"bare://x", "r+", REPORT_ERRORS, &mut opened, None).unwrap();
    assert!(s.read_vec(10).is_none());
    assert!(s.write(b"abc").is_err());
    assert!(s.stat().is_none());
    // the liveness check without stream_eof() assumes EOF
    assert!(s.eof());
    s.close();
    assert_eq!(
        msgs(),
        [
            "VariableStream::stream_read is not implemented!",
            "VariableStream::stream_write is not implemented!",
            "VariableStream::stream_stat is not implemented!",
            "VariableStream::stream_eof is not implemented! Assuming EOF",
        ]
    );
    assert!(!crate::file::unlink(b"bare://x", None));
    assert!(crate::open::opendir(b"bare://x", REPORT_ERRORS, None).is_none());
    assert_eq!(
        msgs(),
        [
            "VariableStream::unlink is not implemented!",
            "Failed to open directory: \"VariableStream::dir_opendir\" is not implemented",
        ]
    );
    wrapper::reset_volatile();
}

#[test]
fn open_failures_and_registration() {
    wrapper::reset_volatile();
    let _ = diag::take();
    setup(b"fail", &["stream_open"], Value::Bool(false));
    let mut opened = None;
    assert!(open::open_wrapper(b"fail://x", "r", REPORT_ERRORS, &mut opened, None).is_none());
    assert_eq!(
        msgs(),
        ["Failed to open stream: \"VariableStream::stream_open\" call failed"]
    );
    setup(b"noopen", &[], Value::Bool(true));
    assert!(open::open_wrapper(b"noopen://x", "r", REPORT_ERRORS, &mut opened, None).is_none());
    assert_eq!(
        msgs(),
        ["Failed to open stream: \"VariableStream::stream_open\" is not implemented"]
    );
    let class = Rc::new(Class {
        shared: Rc::default(),
        implement: FULL,
        open_result: Value::Bool(true),
    });
    assert_eq!(register(b"fail", class.clone(), 0), RegisterResult::Failed);
    assert_eq!(register(b"bad proto", class, 0), RegisterResult::Failed);
    assert_eq!(
        msgs(),
        [
            "Protocol fail:// is already defined.",
            "Invalid protocol scheme specified. Unable to register wrapper class VariableStream to bad proto://",
        ]
    );
    assert!(unregister(b"fail"));
    assert!(!unregister(b"fail"));
    assert!(!restore(b"nonexistent"));
    assert!(restore(b"data"));
    assert_eq!(
        msgs(),
        [
            "Unable to unregister protocol fail://",
            "nonexistent:// never existed, nothing to restore",
            "data:// was never changed, nothing to restore",
        ]
    );
    assert!(unregister(b"file"));
    assert!(restore(b"file"));
    assert!(open::fopen(b"/etc/passwd", "r", false, None).is_some());
    wrapper::reset_volatile();
}

#[test]
fn directories_and_url_stat() {
    wrapper::reset_volatile();
    setup(b"vdir", FULL, Value::Bool(true));
    let mut d = crate::open::opendir(b"vdir://x", REPORT_ERRORS, None).unwrap();
    assert_eq!(d.ops_label(), "user-space-dir");
    assert_eq!(d.readdir().unwrap(), b"entry1");
    assert_eq!(d.readdir().unwrap(), b"entry2");
    assert!(d.readdir().is_none());
    assert!(crate::file::unlink(b"vdir://x", None));
    let r = crate::filestat::php_stat(b"vdir://x", crate::filestat::FsType::Size);
    assert_eq!(r, crate::filestat::StatResult::Int(0));
    wrapper::reset_volatile();
}

#[test]
fn recursion_guard() {
    // An opener re-entering itself with the same URL is refused.
    state::set_user_stream_current_filename(Some(b"var://same".to_vec()));
    wrapper::reset_volatile();
    setup(b"var", FULL, Value::Bool(true));
    let _ = diag::take();
    let mut opened = None;
    assert!(open::open_wrapper(b"var://same", "r", REPORT_ERRORS, &mut opened, None).is_none());
    assert_eq!(
        msgs(),
        ["Failed to open stream: infinite recursion prevented"]
    );
    state::set_user_stream_current_filename(None);
    wrapper::reset_volatile();
}
