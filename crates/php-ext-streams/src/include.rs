//! `include`/`require` through stream wrappers
//! (`php_stream_open_for_zend_ex()`): installed as the VM's include opener.
//!
//! Plain paths are left to the VM unless `include_path` contains wrapper
//! entries. Openers installed later (e.g. phar) chain to this one.

use std::cell::Cell;

use php_runtime::ZStr;
use php_streams::diag::{self, Severity};
use php_streams::stream::{BufferMode, StreamOption};
use php_streams::wrapper::{REPORT_ERRORS, STREAM_OPEN_FOR_INCLUDE, USE_PATH};
use php_vm::ext::{IncludeOpen, IncludeOpenerFn, VmResult};
use php_vm::Vm;

use crate::bridge;

thread_local! {
    static PREV: Cell<Option<IncludeOpenerFn>> = const { Cell::new(None) };
}

/// Installs the opener, chaining one installed before.
pub fn install(vm: &mut Vm) {
    PREV.with(|p| p.set(vm.include_opener));
    vm.set_include_opener(opener);
}

/// Whether `p` names a URL (`scheme://...` or `data:`).
fn is_url(p: &[u8]) -> bool {
    let n = php_streams::wrapper::scheme_len(p);
    n > 1
        && p.get(n) == Some(&b':')
        && (p[n + 1..].starts_with(b"//") || (n == 4 && p[..4].eq_ignore_ascii_case(b"data")))
}

fn opener(vm: &mut Vm, name: &[u8], kind: &str) -> VmResult<IncludeOpen> {
    let wrapped_path = vm
        .ini_string("include_path")
        .is_some_and(|ip| php_streams::util::find_bytes(&ip, b"://").is_some());
    if !is_url(name) && !wrapped_path {
        return match PREV.with(Cell::get) {
            Some(prev) => prev(vm, name, kind),
            None => Ok(IncludeOpen::NotHandled),
        };
    }
    let r = bridge::enter(vm, || {
        let mut opened = None;
        let s = php_streams::open::open_wrapper(
            name,
            "rb",
            USE_PATH | REPORT_ERRORS | STREAM_OPEN_FOR_INCLUDE,
            &mut opened,
            None,
        );
        s.map(|mut s| {
            // `php_stream_open_for_zend_ex`: read buffering is turned off
            // (no double buffering), then `zend_stream_fixup` sizes the
            // stream (`php_zend_stream_fsizer`: a stat unless read filters
            // are attached). User wrappers see both calls.
            s.set_option(&mut StreamOption::ReadBuffer(BufferMode::None, None));
            if s.read_filters.is_empty() {
                let _ = s.stat();
            }
            let src = s.copy_to_mem(None).unwrap_or_default();
            let opened = opened.or_else(|| s.orig_path.clone());
            if bridge::pending_is_fatal() {
                // A fatal error in a wrapper callback longjmps past the
                // file handle's destructor: the stream stays in
                // `EG(regular_list)` and its `stream_close()` runs at
                // request shutdown (`zend_close_rsrc_list`, GH-19844).
                crate::res::park_bailed(s);
            } else {
                s.close();
            }
            (src, opened)
        })
    });
    let pending = bridge::take_pending();
    for d in diag::take() {
        if pending.is_some() && d.code == diag::StreamErrorCode::OpenFailed {
            continue;
        }
        let level = match d.severity {
            Severity::Warning => 2,
            Severity::Notice => 8,
            Severity::Deprecated => 8192,
            Severity::Error => 1,
        };
        let origin = if pending.is_some() {
            active_function_origin(vm)
        } else {
            vm.include_error_origin(kind, name)
        };
        vm.raise(level, &format!("{origin}: {}", d.text()))?;
    }
    if let Some(t) = pending {
        return Err(t);
    }
    Ok(match r {
        Some((source, opened)) => IncludeOpen::Opened {
            source,
            opened_path: ZStr::from(opened.unwrap_or_else(|| name.to_vec())),
        },
        None => IncludeOpen::Failed,
    })
}

/// `php_verror()` origin once an exception is pending: the throw moved the
/// opline off `ZEND_INCLUDE_OR_EVAL`, so the active function names it
/// (`main` at the top level of a script, include or eval).
fn active_function_origin(vm: &Vm) -> String {
    let name = match vm.frames.last() {
        Some(f) if f.kind == php_vm::vm::FrameKind::Function => {
            let n = f
                .func
                .as_ref()
                .map_or_else(|| "main".to_string(), |func| func.name().to_string_lossy());
            match &f.scope {
                Some(c) => format!(
                    "{}::{n}",
                    php_vm::object::display_name(&c.name).to_string_lossy()
                ),
                None => n,
            }
        }
        _ => "main".to_string(),
    };
    format!("{name}()")
}

#[cfg(test)]
mod tests {
    use super::is_url;
    use php_vm::ext::Extension;
    use php_vm::{Vm, VmOptions};
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Sink(Rc<RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// `php_stream_open_for_zend_ex` + `zend_stream_fixup`: a user wrapper
    /// opened for `require` sees `stream_set_option` (no read buffer) and
    /// `stream_stat` (sizing) before the first `stream_read`, whose
    /// exception propagates (`Zend/tests/exceptions/exception_stream_wrapper`).
    #[test]
    fn include_through_a_user_wrapper_sets_option_and_stats_first() {
        let sink = Sink::default();
        let streams = crate::Streams;
        let exts: [&dyn Extension; 1] = [&streams];
        let mut vm = Vm::with_extensions(&VmOptions { ini: vec![] }, Box::new(sink.clone()), &exts);
        vm.run_script(
            br#"<?php
class Loader {
    public $context;
    function stream_open() { return true; }
    function stream_read() { echo "stream_read\n"; throw new Exception("Message"); }
    function __call($m, $a) { echo "$m\n"; }
}
stream_wrapper_register('abc', 'Loader');
try { require 'abc://'; } catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), "\n"; }
"#,
            "test.php",
        );
        drop(vm);
        let out = String::from_utf8_lossy(&sink.0.borrow()).into_owned();
        assert_eq!(
            out,
            "stream_set_option\nstream_stat\nstream_read\nException: Message\n"
        );
    }

    fn run(src: &[u8]) -> String {
        let sink = Sink::default();
        let streams = crate::Streams;
        let exts: [&dyn Extension; 1] = [&streams];
        let mut vm = Vm::with_extensions(&VmOptions { ini: vec![] }, Box::new(sink.clone()), &exts);
        vm.run_script(src, "test.php");
        drop(vm);
        let bytes = sink.0.borrow().clone();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// GH-19844: a fatal error inside a wrapper callback leaves the included
    /// streams open; each one's `stream_close()` runs at request shutdown,
    /// and its own fatal error does not stop the next close.
    #[test]
    fn bailout_leaves_include_streams_for_shutdown() {
        let out = run(br#"<?php
class Test {
    public $context;
    private static $nested = false;
    function stream_open() { return true; }
    function stream_read() { return '.'; }
    function stream_set_option() {}
    function stream_stat() {}
    function stream_eof() {
        if (!Test::$nested) {
            Test::$nested = true;
            include 'Test://';
        }
        @trigger_error('Bail', E_USER_ERROR);
    }
    function stream_close() {
        @trigger_error('Bail', E_USER_ERROR);
    }
}
stream_wrapper_register('Test', Test::class);
include 'Test://';
"#);
        assert_eq!(out.matches("Fatal error: Bail").count(), 3, "{out}");
    }

    /// GH-20286: streams an `include` inside `stream_close()` leaves open
    /// at shutdown are closed as well.
    #[test]
    fn shutdown_closes_streams_parked_while_closing() {
        let out = run(br#"<?php
class lib {
   public $context;
   function stream_set_option() {}
   function stream_stat() { return true; }
   function stream_open() { return true; }
   function stream_read($count) {
     function a() {}
     include('lib://');
   }
   function stream_close() {
     static $count = 0;
     if ($count++ < 3) include('lib://');
   }
}
stream_wrapper_register('lib', lib::class);
include('lib://test.php');
"#);
        assert_eq!(
            out.matches("Fatal error: Cannot redeclare function a()")
                .count(),
            4,
            "{out}"
        );
    }

    #[test]
    fn urls() {
        assert!(is_url(b"test://x"));
        assert!(is_url(b"data:text/plain,x"));
        assert!(!is_url(b"/tmp/x.php"));
        assert!(!is_url(b"c:/x"));
    }
}
