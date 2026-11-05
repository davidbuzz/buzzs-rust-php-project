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
            s.close();
            (src, opened.or_else(|| s.orig_path.clone()))
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
    /// exception propagates (Zend/tests/exceptions/exception_stream_wrapper).
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

    #[test]
    fn urls() {
        assert!(is_url(b"test://x"));
        assert!(is_url(b"data:text/plain,x"));
        assert!(!is_url(b"/tmp/x.php"));
        assert!(!is_url(b"c:/x"));
    }
}
