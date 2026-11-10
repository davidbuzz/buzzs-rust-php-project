//! `include`/`require` through phar: the VM's include opener
//! (`phar_resolve_path()` + `php_stream_open_for_zend_ex()` for `phar://`
//! URLs and `phar://` include paths), source filter
//! (`phar_compile_file()` for phars included or run directly) and the
//! `require` of a PHP entry served by `Phar::webPhar()`.

use std::cell::Cell;

use php_ext_streams::bridge;
use php_phar::include::{self as pinc, CompileSource};
use php_runtime::ZStr;
use php_streams::diag::{self, Severity};
use php_streams::wrapper::{REPORT_ERRORS, STREAM_OPEN_FOR_INCLUDE, USE_PATH};
use php_vm::ext::{IncludeOpen, IncludeOpenerFn, SourceFilterFn, Throw, VmResult};
use php_vm::Vm;

use crate::util::executed;

thread_local! {
    static PREV_OPENER: Cell<Option<IncludeOpenerFn>> = const { Cell::new(None) };
    static PREV_FILTER: Cell<Option<SourceFilterFn>> = const { Cell::new(None) };
    /// The construct of the include being opened (`include`,
    /// `require_once`...), for warnings raised by the source filter.
    static KIND: Cell<&'static str> = const { Cell::new("include") };
}

fn kind_static(kind: &str) -> &'static str {
    ["include", "include_once", "require", "require_once"]
        .into_iter()
        .find(|k| *k == kind)
        .unwrap_or("include")
}

/// Installs the hooks, chaining any opener/filter installed before.
pub fn install(vm: &mut Vm) {
    PREV_OPENER.with(|c| c.set(vm.include_opener));
    PREV_FILTER.with(|c| c.set(vm.source_filter));
    vm.set_include_opener(opener);
    vm.set_source_filter(filter);
}

fn is_phar_url(p: &[u8]) -> bool {
    p.len() >= 7 && p[..7].eq_ignore_ascii_case(b"phar://")
}

fn has_scheme(p: &[u8]) -> bool {
    let n = p
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
        .count();
    n > 1 && p[n..].starts_with(b"://")
}

/// The `E_*` level of a stream diagnostic.
fn level_of(severity: Severity) -> i64 {
    match severity {
        Severity::Warning => 2,
        Severity::Notice => 8,
        Severity::Deprecated => 8192,
        Severity::Error => 1,
    }
}

/// Emits the queued stream diagnostics as `include(): message`.
fn report(vm: &mut Vm, kind: &str) -> VmResult<()> {
    for d in diag::take() {
        let mut msg = format!("{kind}(): ").into_bytes();
        msg.extend_from_slice(&d.message);
        vm.raise_bytes(level_of(d.severity), &msg)?;
    }
    Ok(())
}

/// Parks a throw that must end the running native method once it returns
/// (the first one wins, as the engine keeps the first exception).
fn park(vm: &mut Vm, t: Throw) {
    if vm.pending_throw.is_none() {
        vm.pending_throw = Some(t);
    }
}

/// `zend_compile_file()` could not open a `webPhar()` entry: the stream
/// diagnostics are reported from the running method
/// (`Phar::webPhar(url): message`, `php_stream_display_wrapper_errors`)
/// and `ZMSG_FAILED_REQUIRE_FOPEN` throws.
fn web_open_failed(vm: &mut Vm, url: &[u8]) -> Throw {
    for d in diag::take() {
        let mut msg = b"Phar::webPhar(".to_vec();
        msg.extend_from_slice(url);
        msg.extend_from_slice(b"): ");
        msg.extend_from_slice(&d.message);
        if let Err(t) = vm.raise_bytes(level_of(d.severity), &msg) {
            return t;
        }
    }
    let ip = vm.ini_string("include_path").unwrap_or_default();
    vm.throw(
        "Error",
        &format!(
            "Failed opening required '{}' (include_path='{}')",
            String::from_utf8_lossy(url),
            String::from_utf8_lossy(&ip)
        ),
    )
}

/// `phar_file_action()` for `PHAR_MIME_PHP` (`Phar::webPhar()`): runs the
/// archive entry `url` (`phar://<archive>/<entry>`) the way php-src does,
/// `zend_compile_file(ZEND_REQUIRE)` + `zend_execute()` from the
/// `webPhar()` frame: read through the `phar://` wrapper, compiled as a
/// file named by the URL and executed at the top level with the global
/// symbol table. Returns whether it ran: an entry already in
/// `EG(included_files)` is skipped and the stub goes on. Whatever ends the
/// run (`exit`, an uncaught exception, a fatal error), or the `require`
/// error of an entry that cannot be opened, is parked in
/// [`Vm::pending_throw`] for `webPhar()` to return.
pub(crate) fn run_web_entry(vm: &mut Vm, url: &[u8]) -> bool {
    let path = ZStr::from(url);
    if !vm.included_set.insert(path.clone()) {
        return false;
    }
    vm.included.push(path.clone());
    // zend_stream_open(): the entry is read through the wrapper.
    let source = bridge::enter(vm, || {
        let mut opened = None;
        let s = php_streams::open::open_wrapper(
            url,
            "rb",
            USE_PATH | REPORT_ERRORS | STREAM_OPEN_FOR_INCLUDE,
            &mut opened,
            None,
        );
        s.map(|mut s| {
            let src = s.copy_to_mem(None).unwrap_or_default();
            s.close();
            src
        })
    });
    if let Some(t) = bridge::take_pending() {
        park(vm, t);
        return false;
    }
    let Some(source) = source else {
        // zend_compile_file() failed: the entry leaves EG(included_files).
        vm.included_set.remove(&path);
        if vm.included.last() == Some(&path) {
            vm.included.pop();
        }
        let t = web_open_failed(vm, url);
        park(vm, t);
        return false;
    };
    // The entry is not the primary script: `run_main` is the top-level
    // runner (file compilation, a frame over the globals), so the script
    // state it records is kept.
    let script_path = vm.script_path.clone();
    let main_return = vm.main_return.clone();
    // `zend_execute()` from inside `Phar::webPhar()`: the entry's errors
    // and exceptions surface through the builtin's frame.
    let r = vm.run_nested_script(&source, &path);
    vm.script_path = script_path;
    vm.main_return = main_return;
    if let Err(t) = r {
        park(vm, t);
    }
    true
}

/// Opens `target` (with `include_path` resolution) and reads it.
fn open(vm: &mut Vm, target: &[u8], kind: &str) -> VmResult<IncludeOpen> {
    let r = bridge::enter(vm, || {
        let mut opened = None;
        let s = php_streams::open::open_wrapper(
            target,
            "rb",
            USE_PATH | REPORT_ERRORS | STREAM_OPEN_FOR_INCLUDE,
            &mut opened,
            None,
        );
        s.map(|mut s| {
            let src = s.copy_to_mem(None).unwrap_or_default();
            s.close();
            (src, opened)
        })
    });
    if let Some(t) = bridge::take_pending() {
        return Err(t);
    }
    report(vm, kind)?;
    Ok(match r {
        Some((source, opened)) => {
            let path = opened.unwrap_or_else(|| target.to_vec());
            IncludeOpen::Opened {
                source,
                opened_path: ZStr::from(path),
            }
        }
        None => IncludeOpen::Failed,
    })
}

/// The include opener.
fn opener(vm: &mut Vm, name: &[u8], kind: &str) -> VmResult<IncludeOpen> {
    KIND.with(|k| k.set(kind_static(kind)));
    if name.is_empty() {
        return chain(vm, name, kind);
    }
    if is_phar_url(name) {
        return open(vm, name, kind);
    }
    if has_scheme(name) {
        return chain(vm, name, kind);
    }
    let ex = executed(vm);
    let inc = vm.ini_string("include_path").unwrap_or_default();
    let in_phar = ex.as_deref().is_some_and(is_phar_url);
    if in_phar {
        // phar_resolve_path(): relative to the running phar first.
        let resolved = bridge::enter(vm, || pinc::resolve_path(name, ex.as_deref(), &inc));
        let _ = diag::take();
        if let Some(url) = resolved {
            return open(vm, &url, kind);
        }
    }
    let wrapped_path = inc.windows(3).any(|w| w == b"://");
    if in_phar || wrapped_path {
        return open(vm, name, kind);
    }
    chain(vm, name, kind)
}

fn chain(vm: &mut Vm, name: &[u8], kind: &str) -> VmResult<IncludeOpen> {
    match PREV_OPENER.with(Cell::get) {
        Some(f) => f(vm, name, kind),
        None => Ok(IncludeOpen::NotHandled),
    }
}

/// The source filter: what to compile for a phar included or run
/// directly.
fn filter(vm: &mut Vm, path: &ZStr, src: Vec<u8>) -> VmResult<Vec<u8>> {
    let src = match PREV_FILTER.with(Cell::get) {
        Some(f) => f(vm, path, src)?,
        None => src,
    };
    let name = path.as_bytes();
    if !pinc::has_marker_in_filename(name) || has_scheme(name) {
        return Ok(src);
    }
    let cs = bridge::enter(vm, || pinc::compile_source(name));
    let _ = diag::take();
    Ok(match cs {
        CompileSource::Original => src,
        CompileSource::Bytes(b) => b,
        CompileSource::Stub(url) => {
            // zend_stream_open_function() on the stub: its errors are
            // reported, and the file itself is compiled when it fails.
            let stub = bridge::enter(vm, || {
                let mut opened = None;
                php_streams::open::open_wrapper(&url, "rb", REPORT_ERRORS, &mut opened, None)
                    .and_then(|mut s| s.copy_to_mem(None))
            });
            if let Some(t) = bridge::take_pending() {
                return Err(t);
            }
            report(vm, KIND.with(Cell::get))?;
            stub.unwrap_or(src)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{has_scheme, is_phar_url};

    #[test]
    fn url_detection() {
        assert!(is_phar_url(b"PHAR://x.phar/a"));
        assert!(!is_phar_url(b"phar:/x"));
        assert!(has_scheme(b"data://text/plain,x"));
        assert!(!has_scheme(b"/abs/path"));
        assert!(!has_scheme(b"c:/x"));
    }
}
